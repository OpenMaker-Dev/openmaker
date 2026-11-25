use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use om_hbam::slot::uint_segment;
use om_hbam::{serial, OwnedNode, RidList};
use om_protocol::write::{
    attention, Conflict, CreateRecord, GetSerial, KeyPath, KillAllLocks, Lock, LockReply, Unlock,
    COMMIT_DELETED, COMMIT_NEW, LOCK_CONFLICT, LOCK_WRITE, SERIAL_INCREMENT,
};
use om_scripts::text_node;

use super::*;
use crate::config::HostedFile;
use crate::eventlog::EventLog;
use crate::hosting::Hosting;
use crate::modcount::{serial_leaf, MOD_COUNT};
use crate::scripts::fixture;

/// A fixed clock, a counter for a random source and a virtual stopwatch,
/// the same for every engine made with it.
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

    fn stopwatch(&self) -> om_data::rules::Stopwatch {
        virtual_stopwatch(Duration::from_millis(1), Arc::default())
    }
}

/// A stopwatch on virtual time, as a seeded environment's
/// ([`Environment::stopwatch`]): its n-th reading, from 0, answers `n`
/// times `step`, counted from its own start, so that each stopwatch
/// measures its own operation's time whatever others were read. `readings`
/// counts the readings of every stopwatch it is given to, in total.
fn virtual_stopwatch(step: Duration, readings: Arc<AtomicU64>) -> om_data::rules::Stopwatch {
    let own = AtomicU64::new(0);
    Arc::new(move || {
        readings.fetch_add(1, Ordering::Relaxed);
        let n = own.fetch_add(1, Ordering::Relaxed);
        step * u32::try_from(n).unwrap_or(u32::MAX)
    })
}

fn engine() -> FileEngine {
    FileEngine::new(Box::new(Seeded(AtomicU64::new(0))))
}

/// [`Seeded`], its counter shared with the test, which reads and sets it
/// to draw again what an attempt drew.
struct Rewound(Arc<AtomicU64>);

impl Environment for Rewound {
    fn now(&self) -> jiff::Zoned {
        "2026-11-24T12:00:00[UTC]".parse().unwrap()
    }

    fn fill_random(&self, bytes: &mut [u8]) {
        for byte in bytes {
            *byte = self.0.fetch_add(1, Ordering::Relaxed) as u8;
        }
    }

    fn stopwatch(&self) -> om_data::rules::Stopwatch {
        virtual_stopwatch(Duration::from_millis(1), Arc::default())
    }
}

/// What a step whose save stopped before its rename leaves as it was
/// (docs/server-storage.md 5, "not stored"): what readers see of the tree,
/// the schema read from it, the basis of stored calculations, the
/// accounts, what reads left waiting, the lock table, whether the file
/// holds changes the disk lacks, and the file on disk, with no copy left
/// beside it.
#[derive(Debug, PartialEq)]
struct Untouched {
    tree: OwnedNode,
    catalog: om_hbam::Catalog,
    basis: std::collections::HashMap<Vec<u8>, crate::recalc::TableBasis>,
    accounts: Vec<crate::hosting::FileAccount>,
    waiting: (Vec<crate::store::IndexId>, Vec<crate::store::MemberKey>),
    locks: Vec<(KeyPath, LockUser, u16, u32, String)>,
    unsaved: bool,
    disk: Vec<u8>,
    copy_left: bool,
}

fn untouched(file: &Hosted, disk: &fixture::TempFile) -> Untouched {
    let store = file.read_store();
    let copy = crate::store::save_copy_path(&disk.0);
    Untouched {
        tree: store.node(&[]).unwrap().clone(),
        catalog: store.catalog().clone(),
        basis: store.basis_for_tests().clone(),
        accounts: file.accounts().iter().map(|a| (**a).clone()).collect(),
        waiting: store.waiting_for_tests(),
        locks: file.lock_table().entries(),
        unsaved: store.has_unsaved_changes(),
        disk: std::fs::read(&disk.0).unwrap(),
        copy_left: std::fs::symlink_metadata(copy).is_ok(),
    }
}

/// Whether `reply` is its operation's storage error for a save that failed
/// with FileMaker's error 800 (docs/server-storage.md 5, the table of
/// replies).
fn is_storage_error(reply: &Reply) -> bool {
    use crate::hosting::{AccountEditError, PasswordChangeError};
    const ERROR: u16 = 800;
    match reply {
        Reply::ReserveRecordIds(r) => r.status == u32::from(ERROR),
        Reply::AdvanceSerial(r) => r.status == u32::from(ERROR) && r.value.is_empty(),
        Reply::CommitRecords(r) => matches!(r.result, Err(CommitError::NotSaved(_))),
        Reply::UploadToCatalog(r) => matches!(r, Err(e) if e.code == ERROR && !e.unknown),
        Reply::ChangeSchema(r) => {
            matches!(r, Err(SchemaNotChanged::NotStored(e)) if e.code == ERROR && !e.unknown)
        }
        Reply::StoreIndexes(r) => matches!(r, Err(e) if e.code == ERROR),
        Reply::UpdateTable(r) => matches!(r, Err(e) if e.contains("could not be saved")),
        Reply::StoreContainerData(r) => r.status == ERROR && r.member.is_empty(),
        Reply::AdjustContainerCount(count) => *count == COUNT_FAILED,
        Reply::EditDesign(edited) => matches!(
            &edited.reply,
            design::Reply::Refused { status, .. } if *status == ERROR
        ),
        Reply::SaveFile(r) => matches!(r, Err(FileNotSaved::Failed(e)) if e.code == ERROR),
        Reply::CommitScriptRecords(r) => matches!(r, Err((code, _)) if *code == u32::from(ERROR)),
        Reply::SetNextSerial(r) => *r == Err(u32::from(ERROR)),
        Reply::ChangePassword(r) => matches!(r, Err(PasswordChangeError::Unwritten(_))),
        Reply::ResetPassword(r) | Reply::DeleteAccount(r) => {
            matches!(r, Err(AccountEditError::Unwritten(_)))
        }
        Reply::EnableAccount(r) => matches!(r, Err(AccountEditError::Unwritten(_))),
        Reply::SaveFoundSet(kept) => !kept,
        Reply::FinishDeferredWork(r) => matches!(r, Err(e) if e.code == ERROR && !e.unknown),
        _ => false,
    }
}

/// docs/server-storage.md 5: a save that stops before its rename stores
/// nothing of the step that made it, and takes the step back. `steps` were
/// recorded with their outcomes from a fresh load of `tree` by [`engine`],
/// and left the tree `last`. For each step, on a fresh load of `tree` with
/// the steps before it replayed, the step is submitted with every save
/// failing before its rename (`SaveFault::BeforeRename`). One that is
/// answered otherwise than recorded is answered with its storage error and
/// no notices, and leaves everything as it was ([`Untouched`]). Submitted
/// again once saves work, with the same draws from the environment, it is
/// answered as recorded, and the steps after it replay as recorded to
/// `last`. Returns the steps taken back.
fn each_failed_save_takes_its_step_back(
    tag: &str,
    tree: &OwnedNode,
    steps: &[(Operation, Given, Outcome)],
    last: &OwnedNode,
) -> Vec<usize> {
    let mut taken_back = Vec::new();
    'failing: for failing in 0..steps.len() {
        let (disk, file) = host(&format!("{tag}-{failing}"), tree);
        let drawn = Arc::new(AtomicU64::new(0));
        let engine = FileEngine::new(Box::new(Rewound(Arc::clone(&drawn))));
        for (step, (operation, given, outcome)) in steps.iter().enumerate() {
            let submit = || engine.submit_with(&file, operation.clone(), given.credentials());
            if step != failing {
                assert_eq!(
                    &submit(),
                    outcome,
                    "step {step} after {failing}: {operation:?}"
                );
                continue;
            }
            let before = untouched(&file, &disk);
            let draws = drawn.load(Ordering::Relaxed);
            file.fail_saves(Some(crate::store::SaveFault::BeforeRename));
            let failed = submit();
            file.fail_saves(None);
            if &failed == outcome {
                // Nothing of its own to store, or nothing saved.
                continue;
            }
            taken_back.push(step);
            assert!(
                is_storage_error(&failed.reply),
                "step {step}: {operation:?}: {:?}",
                failed.reply
            );
            assert!(failed.notices.changes.is_empty(), "step {step}: notices");
            assert_eq!(
                untouched(&file, &disk),
                before,
                "step {step}: {operation:?}"
            );
            if *operation == Operation::FinishDeferredWork {
                // Its work list is back, and the file is served read-only,
                // as when the work cannot be done: nothing after it runs
                // as recorded.
                assert!(engine.read_only(&file).is_some(), "step {step}: read-only");
                continue 'failing;
            }
            drawn.store(draws, Ordering::Relaxed);
            assert_eq!(&submit(), outcome, "step {step} again: {operation:?}");
        }
        assert_eq!(
            file.read_store().node(&[]),
            Some(last),
            "the tree after the steps, step {failing} failing once"
        );
    }
    taken_back
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
    // op71, both op72s and op70 save; each is taken back when its save
    // fails, and done when submitted again.
    let steps: Vec<_> = recorded
        .iter()
        .map(|(operation, outcome)| (operation.clone(), Given::Nothing, outcome.clone()))
        .collect();
    let last = file.read_store().node(&[]).unwrap().clone();
    let taken_back = each_failed_save_takes_its_step_back("engine-failing", &tree, &steps, &last);
    assert_eq!(taken_back, [0, 3, 4, 8]);
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
    assert_eq!(outcome.reply, Reply::StoreIndexes(Ok(())));
    assert_eq!(postings(&file, &index(0)), (values, (3, 3)));

    let upload = UploadToCatalog {
        path: deferred.clone(),
        node: work,
    };
    let (_, outcome) = submit(Step::Operation(Operation::UploadToCatalog(upload)));
    assert_eq!(outcome.reply, Reply::UploadToCatalog(Ok(true)));
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
    let steps: Vec<_> = recorded
        .iter()
        .map(|(operation, outcome, _)| (operation.clone(), Given::Nothing, outcome.clone()))
        .collect();
    let last = recorded.last().unwrap().2.clone();
    let taken_back =
        each_failed_save_takes_its_step_back("engine-schema-failing", &tree, &steps, &last);
    // The find's indexes, op13, op73 and op86's writes with the member key
    // and the second find's index.
    assert_eq!(taken_back, [0, 1, 2, 3]);
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
    let steps: Vec<_> = recorded
        .iter()
        .map(|(operation, outcome, _, _)| (operation.clone(), Given::Nothing, outcome.clone()))
        .collect();
    let last = recorded.last().unwrap().2.clone();
    let taken_back =
        each_failed_save_takes_its_step_back("engine-design-failing", &tree, &steps, &last);
    // Every edit, op240's Save (which reports the failure; the script's
    // reservation before it saves nothing), op88 and op87.
    let edits = 0..19;
    assert_eq!(taken_back, edits.chain([20, 21, 22]).collect::<Vec<_>>());
}

/// The state an operation leaves.
#[derive(Clone, Debug, PartialEq)]
struct State {
    tree: OwnedNode,
    /// The lock table (`LockTable::entries`).
    locks: Vec<(KeyPath, LockUser, u16, u32, String)>,
    /// How many times the file was saved.
    saves: u64,
    /// Whether it holds changes the disk lacks.
    unsaved: bool,
}

fn state(file: &Hosted) -> State {
    let store = file.read_store();
    State {
        tree: store.node(&[]).unwrap().clone(),
        locks: file.lock_table().entries(),
        saves: store.saves(),
        unsaved: store.has_unsaved_changes(),
    }
}

/// docs/server-storage.md 5, 7.3: a server script's writes and locks, and
/// the release of a session's locks, are operations too, so a recorded
/// sequence of them replays against a fresh load to the same replies,
/// notices, tree, lock table and saves. A script session reserves record
/// 4 and locks record 2 and a Notes record; a client locks record 3, which
/// the script is then refused (the conflict a step answers with 301); the
/// script commits a new record 4 with container data, an edit of 2 and the
/// deletion of 1, which releases the photo 1 held, and keeps its lock on
/// the Notes record; its commit of 3 is refused; it locks and
/// releases the accounts catalog, telling of a change; it sets a field's
/// next serial value, reserves record 5 and ends, its locks released and
/// the reservation saved; the client's session ends. Each effect is checked
/// against the fixture, and on replay every step but the refusals must
/// change the tree, the lock table or the saves, and the refusals nothing.
#[test]
fn a_recorded_sequence_of_script_operations_replays_to_the_same_replies_and_locks() {
    use crate::library::{self, LIBRARY};
    use crate::writes::RecordWrite;
    use om_data::containers::reference_text;
    /// The container data the script sets in record 4's photo.
    const SCRIPT_FILE: &[u8] = b"a script's file";
    let mut tree = fixture::tree(&[], true);
    fixture::add_tasks(&mut tree, 1..=3);
    fixture::add_photo(&mut tree);
    let (_first, file) = host("engine-script-recorded", &tree);
    let script = LockUser {
        connection: 0x4000_0001,
        txn: 1,
    };
    let client = LockUser {
        connection: 2,
        txn: 9,
    };
    let script_holder = "OpenMaker (script engine, Admin)";
    let client_holder = "C (Admin)";
    let table = uint_segment(fixture::TABLE);
    let record = |id: u64| vec![table.clone(), vec![0x05], serial::encode(id)];
    let member = |id: u64| vec![table.clone(), vec![LIBRARY], vec![5], serial::encode(id)];
    let photo = |id: u64| {
        let mut path = record(id);
        path.push(uint_segment(fixture::PHOTO));
        path
    };
    let reserve = || {
        Operation::ReserveRecordIds(ReserveRecordIds {
            user: script,
            holder: script_holder.into(),
            request: CreateRecord {
                table: vec![table.clone()],
                count: 1,
                txn: script.txn,
            },
            room: writes::UNCAPPED,
            durable: false,
        })
    };
    let script_lock = |path: KeyPath| {
        Operation::TakeScriptLock(TakeScriptLock {
            user: script,
            holder: script_holder.into(),
            path,
        })
    };
    // A record the script holds and does not commit: its lock outlasts the
    // commit.
    let kept = vec![uint_segment(fixture::NOTES), vec![0x05], serial::encode(1)];
    let catalog = vec![vec![23], vec![1]];
    let catalog_changes = vec![
        Change {
            path: catalog.clone(),
            code: 2,
        },
        Change {
            path: vec![vec![23], vec![1], vec![1]],
            code: 2,
        },
    ];
    let new = OwnedNode::list(
        serial::encode(4),
        vec![
            text_node(&uint_segment(fixture::NAME), "Task 4"),
            serial_leaf(MOD_COUNT, 1),
        ],
    );
    let mut edited = node_at(&file, &record(2)).unwrap();
    edited.set_child(text_node(&uint_segment(fixture::NAME), "Task two"));
    edited.set_child(serial_leaf(MOD_COUNT, 1));
    let commit = CommitScriptRecords {
        user: script,
        records: vec![
            RecordWrite {
                table: fixture::TABLE,
                id: 4,
                record: Some(new),
                new: true,
                uploads: vec![(fixture::PHOTO, file_data(SCRIPT_FILE))],
            },
            RecordWrite {
                table: fixture::TABLE,
                id: 2,
                record: Some(edited.clone()),
                new: false,
                uploads: Vec::new(),
            },
            RecordWrite {
                table: fixture::TABLE,
                id: 1,
                record: None,
                new: false,
                uploads: Vec::new(),
            },
        ],
        bulk_delete: false,
    };
    let mut held = node_at(&file, &record(3)).unwrap();
    held.set_child(text_node(&uint_segment(fixture::NAME), "Task three"));
    let refused_commit = CommitScriptRecords {
        user: script,
        records: vec![RecordWrite {
            table: fixture::TABLE,
            id: 3,
            record: Some(held),
            new: false,
            uploads: Vec::new(),
        }],
        bulk_delete: false,
    };
    let serial_path = [
        table.clone(),
        vec![3],
        vec![5],
        uint_segment(fixture::COUNT),
        vec![0x0c],
    ];
    // The steps, in order.
    const RESERVE_4: usize = 0;
    const LOCK_2: usize = 1;
    const LOCK_KEPT: usize = 2;
    const CLIENT_LOCK_3: usize = 3;
    const REFUSED_LOCK_3: usize = 4;
    const COMMIT: usize = 5;
    const REFUSED_COMMIT: usize = 6;
    const LOCK_ACCOUNTS: usize = 7;
    const RELEASE_ACCOUNTS: usize = 8;
    const SET_SERIAL: usize = 9;
    const RESERVE_5: usize = 10;
    const SCRIPT_END: usize = 11;
    const SAVE: usize = 12;
    const CLIENT_END: usize = 13;
    let operations = vec![
        reserve(),
        script_lock(record(2)),
        script_lock(kept.clone()),
        Operation::TakeLock(TakeLock {
            user: client,
            holder: client_holder.into(),
            request: Lock {
                waiter: 0,
                timeout: 0,
                want_mod_count: false,
                path: record(3),
                argument: 0,
                host_file: 1,
                txn: client.txn,
                lock_type: LOCK_WRITE,
            },
            room: writes::MAX_LOCKS,
        }),
        script_lock(record(3)),
        Operation::CommitScriptRecords(commit),
        Operation::CommitScriptRecords(refused_commit),
        script_lock(catalog.clone()),
        Operation::ReleaseScriptLock(ReleaseScriptLock {
            user: script,
            path: catalog.clone(),
            changes: catalog_changes.clone(),
        }),
        Operation::SetNextSerial(SetNextSerial {
            table: table.clone(),
            field: uint_segment(fixture::COUNT),
            value: "10".into(),
        }),
        reserve(),
        Operation::ReleaseSessionLocks(ReleaseSessionLocks {
            connection: script.connection,
        }),
        Operation::SaveFile,
        Operation::ReleaseSessionLocks(ReleaseSessionLocks {
            connection: client.connection,
        }),
    ];
    assert_eq!(operations.len(), CLIENT_END + 1);
    // The steps that change nothing: the refused lock and commit.
    let refusals = [REFUSED_LOCK_3, REFUSED_COMMIT];
    let recording = engine();
    let mut recorded: Vec<(Operation, Outcome, State)> = Vec::new();
    for operation in operations {
        let outcome = recording.submit(&file, operation.clone());
        recorded.push((operation, outcome, state(&file)));
    }
    let outcome = |step: usize| &recorded[step].1;
    let locks = |step: usize| &recorded[step].2.locks;
    let saves = |step: usize| recorded[step].2.saves;
    let script_lock_on =
        |path: &KeyPath| (path.clone(), script, LOCK_WRITE, 1, script_holder.into());
    let client_lock_on =
        |path: &KeyPath| (path.clone(), client, LOCK_WRITE, 1, client_holder.into());

    // Record 4 reserved and locked for the script, not saved.
    assert!(
        matches!(&outcome(RESERVE_4).reply, Reply::ReserveRecordIds(r) if r.status == 0 && r.record == [serial::encode(4)]),
        "{:?}",
        outcome(RESERVE_4).reply
    );
    assert_eq!(locks(RESERVE_4), &vec![script_lock_on(&record(4))]);
    assert!(
        recorded[RESERVE_4].2.unsaved,
        "the reservation waits unsaved"
    );
    assert_eq!(outcome(LOCK_2).reply, Reply::TakeScriptLock(Ok(())));
    assert_eq!(outcome(LOCK_KEPT).reply, Reply::TakeScriptLock(Ok(())));
    assert!(matches!(
        outcome(CLIENT_LOCK_3).reply,
        Reply::TakeLock(LockReply::Granted { mod_count: None })
    ));
    // The script is refused the record the client holds, with its name.
    assert_eq!(
        outcome(REFUSED_LOCK_3).reply,
        Reply::TakeScriptLock(Err(Conflict {
            self_conflict: false,
            users: client_holder.into(),
        }))
    );
    assert_eq!(
        locks(REFUSED_LOCK_3),
        &vec![
            script_lock_on(&record(2)),
            client_lock_on(&record(3)),
            script_lock_on(&record(4)),
            script_lock_on(&kept),
        ]
    );

    // The commit: record 4 with its name and its photo stored as member
    // 2, record 2 edited, record 1 deleted and the photo it held, member 1,
    // released; the script's locks on 2 and 4 released and the one on the
    // Notes record it did not commit kept; the file saved.
    assert_eq!(outcome(COMMIT).reply, Reply::CommitScriptRecords(Ok(())));
    let code = |kind| kind | attention::RECORD;
    assert_eq!(
        outcome(COMMIT).notices,
        Notices {
            from: script.connection,
            changes: vec![
                Change {
                    path: record(4),
                    code: code(attention::ADD),
                },
                Change {
                    path: record(2),
                    code: code(attention::MODIFY),
                },
                Change {
                    path: record(1),
                    code: code(attention::DELETE),
                },
            ],
        }
    );
    let after_commit = &recorded[COMMIT].2.tree;
    let at = |path: &[Vec<u8>]| {
        let path: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
        after_commit.at(&path).cloned()
    };
    let name =
        |id: u64| at(&record(id)).and_then(|n| crate::recalc::texts(&n).remove(&fixture::NAME));
    assert_eq!(name(4).as_deref(), Some("Task 4"));
    assert_eq!(name(2).as_deref(), Some("Task two"));
    assert_eq!(
        at(&record(2)).map(|n| crate::recalc::texts(&n)),
        Some(crate::recalc::texts(&edited))
    );
    assert_eq!(
        at(&photo(4)).and_then(|n| n.value),
        text_node(&[], &reference_text(&serial::encode(2))).value
    );
    let stored = at(&member(2)).expect("the photo's member");
    assert_eq!(library::reference_count(&stored), 1);
    assert_eq!(
        stored.child(b"FILE"),
        Some(&om_data::containers::stream_node(b"FILE", SCRIPT_FILE)),
        "the bytes the script set"
    );
    assert_eq!(library::member_tree(&stored), Some(file_data(SCRIPT_FILE)));
    assert!(
        at(&member(1)).is_none(),
        "the deleted record's photo is released"
    );
    assert!(at(&record(1)).is_none(), "record 1 is deleted");
    assert_eq!(
        locks(COMMIT),
        &vec![client_lock_on(&record(3)), script_lock_on(&kept)]
    );
    assert_eq!(saves(COMMIT), saves(REFUSED_LOCK_3) + 1);
    assert!(
        !recorded[COMMIT].2.unsaved,
        "the commit saved the reservation too"
    );

    // The commit of the record the client holds is refused with 301, and
    // changes nothing.
    assert!(
        matches!(
            &outcome(REFUSED_COMMIT).reply,
            Reply::CommitScriptRecords(Err((301, _)))
        ),
        "{:?}",
        outcome(REFUSED_COMMIT).reply
    );
    assert!(outcome(REFUSED_COMMIT).notices.changes.is_empty());

    // The accounts catalog: locked, then released telling of the change.
    assert_eq!(outcome(LOCK_ACCOUNTS).reply, Reply::TakeScriptLock(Ok(())));
    assert!(locks(LOCK_ACCOUNTS).contains(&script_lock_on(&catalog)));
    assert_eq!(
        outcome(RELEASE_ACCOUNTS).notices,
        Notices {
            from: script.connection,
            changes: catalog_changes,
        }
    );
    assert_eq!(
        locks(RELEASE_ACCOUNTS),
        &vec![client_lock_on(&record(3)), script_lock_on(&kept)]
    );

    // Set Next Serial Value: stored and saved.
    assert_eq!(outcome(SET_SERIAL).reply, Reply::SetNextSerial(Ok(())));
    let path: Vec<&[u8]> = serial_path.iter().map(Vec::as_slice).collect();
    assert_eq!(
        recorded[SET_SERIAL]
            .2
            .tree
            .at(&path)
            .and_then(|n| n.value.clone()),
        Some(om_scsu::encode_scsu("10"))
    );
    assert_eq!(saves(SET_SERIAL), saves(RELEASE_ACCOUNTS) + 1);

    // Record 5 reserved; the script's end releases its locks, and the save
    // after it writes the reservation.
    assert!(
        matches!(&outcome(RESERVE_5).reply, Reply::ReserveRecordIds(r) if r.record == [serial::encode(5)]),
        "{:?}",
        outcome(RESERVE_5).reply
    );
    assert!(locks(RESERVE_5).contains(&script_lock_on(&record(5))));
    assert_eq!(locks(SCRIPT_END), &vec![client_lock_on(&record(3))]);
    assert_eq!(outcome(SAVE).reply, Reply::SaveFile(Ok(())));
    assert_eq!(saves(SAVE), saves(SCRIPT_END) + 1);
    assert!(!recorded[SAVE].2.unsaved);
    // The client's session ends: no lock is left.
    assert!(locks(CLIENT_END).is_empty());
    let noticed: Vec<usize> = (0..recorded.len())
        .filter(|step| !outcome(*step).notices.changes.is_empty())
        .collect();
    assert_eq!(
        noticed,
        [COMMIT, RELEASE_ACCOUNTS],
        "only the commit and the accounts tell"
    );

    let (_second, again) = host("engine-script-replayed", &tree);
    let replaying = engine();
    let mut before = state(&again);
    for (step, (operation, outcome, after)) in recorded.iter().enumerate() {
        assert_eq!(
            &replaying.submit(&again, operation.clone()),
            outcome,
            "{operation:?}"
        );
        let now = state(&again);
        assert_eq!(&now, after, "the state after {operation:?}");
        if refusals.contains(&step) {
            assert_eq!(now, before, "{operation:?} changed something");
        } else {
            assert_ne!(now, before, "{operation:?} changed nothing");
        }
        before = now;
    }
    let steps: Vec<_> = recorded
        .iter()
        .map(|(operation, outcome, _)| (operation.clone(), Given::Nothing, outcome.clone()))
        .collect();
    let last = recorded.last().unwrap().2.tree.clone();
    let taken_back =
        each_failed_save_takes_its_step_back("engine-script-failing", &tree, &steps, &last);
    // The commit, Set Next Serial Value and the Save at the script's end;
    // the reservations and locks save nothing, and the refused commit
    // stores nothing.
    assert_eq!(taken_back, [COMMIT, SET_SERIAL, SAVE]);
}

/// docs/server-storage.md 5, "unknown": a save that renamed its new copy
/// over the file and could not flush the folder has an unknown outcome.
/// The operation is answered with its storage error and no notices; the
/// tree keeps the change, as the renamed file holds it; the file takes no
/// more changes: each operation that would change it is refused unrun with
/// the storage error, its draws not drawn, while reads, lock requests and
/// their releases go on; one alert is raised, and the file's state says
/// why. Hosting the file again clears it.
#[test]
fn a_save_whose_folder_flush_fails_leaves_the_file_taking_no_more_changes() {
    let mut tree = fixture::tree(&[], true);
    fixture::add_tasks(&mut tree, 1..=3);
    fixture::add_photo(&mut tree);
    let disk = fixture::TempFile::from_tree("engine-unknown", &tree);
    let events = EventLog::new("test".into());
    let hosted = HostedFile {
        name: fixture::FILE.into(),
        path: disk.0.clone(),
    };
    let hosting = Hosting::load(&[hosted], false, &events, &engine()).unwrap();
    let file = hosting.by_name(fixture::FILE).unwrap();
    let drawn = Arc::new(AtomicU64::new(0));
    let engine = FileEngine::new(Box::new(Rewound(Arc::clone(&drawn))));
    let table = uint_segment(fixture::TABLE);
    let user = LockUser {
        connection: 1,
        txn: 7,
    };
    let reserve = || {
        Operation::ReserveRecordIds(ReserveRecordIds {
            user,
            holder: "A (Admin)".into(),
            request: CreateRecord {
                table: vec![table.clone()],
                count: 1,
                txn: user.txn,
            },
            room: writes::MAX_LOCKS,
            durable: true,
        })
    };
    let last_on_disk = || {
        let hbam = om_hbam::HbamFile::open(&disk.0).unwrap();
        let node = hbam.subtree(&[&table, &[0]]).unwrap().unwrap();
        let value = node.value.unwrap();
        serial::decode(&value[1..]).unwrap()
    };
    file.fail_saves(Some(crate::store::SaveFault::FolderFlush));
    let outcome = engine.submit(&file, reserve());
    file.fail_saves(None);
    assert!(
        matches!(&outcome.reply, Reply::ReserveRecordIds(r) if r.status == 800),
        "{:?}",
        outcome.reply
    );
    assert!(outcome.notices.changes.is_empty());
    // The tree keeps the reservation, which the renamed file holds.
    assert_eq!(file.read_store().last_record_id(&table), 4);
    assert_eq!(last_on_disk(), 4);
    assert!(!file.read_store().has_unsaved_changes());
    let why = file
        .writes_refused()
        .expect("the file takes no more changes");
    assert!(why.contains("takes no more changes"), "{why}");
    assert_eq!(events.alerts().len(), 1);

    // Changes are refused unrun, with the storage error.
    let before = untouched(&file, &disk);
    let draws = drawn.load(Ordering::Relaxed);
    let serial = Operation::AdvanceSerial(AdvanceSerial {
        request: GetSerial {
            table: vec![table.clone()],
            field: vec![uint_segment(fixture::COUNT)],
            value: String::new(),
            increment: 1,
            flags: SERIAL_INCREMENT,
        },
    });
    let upload = Operation::UploadToCatalog(UploadToCatalog {
        path: vec![table.clone(), vec![0x03], vec![0x01], vec![0xfd]],
        node: OwnedNode::list(vec![0xfd], Vec::new()),
    });
    let data = Operation::StoreContainerData(StoreContainerData {
        table: vec![table.clone()],
        data: Ok(file_data(b"refused")),
        bytes: 7,
    });
    for operation in [reserve(), serial, upload, data] {
        let outcome = engine.submit(&file, operation.clone());
        assert!(
            is_storage_error(&outcome.reply),
            "{operation:?}: {outcome:?}"
        );
    }
    let saved = engine.submit(&file, Operation::SaveFile);
    assert!(
        matches!(&saved.reply, Reply::SaveFile(Err(FileNotSaved::Unknown(e))) if e.code == 800),
        "{saved:?}"
    );
    let staged = OwnedNode::list(
        serial::encode(1),
        vec![text_node(&uint_segment(fixture::NAME), "Not stored")],
    );
    let record = vec![table.clone(), vec![0x05], serial::encode(1)];
    let commit = Operation::CommitRecords(CommitRecords {
        user,
        account: fixture::ACCOUNT.into(),
        access: file
            .account(fixture::ACCOUNT)
            .unwrap()
            .access
            .clone()
            .unwrap(),
        list: OwnedNode::list(Vec::new(), Vec::new()),
        staged: Staged::from([(record.clone(), staged.clone())]),
        withheld: Withheld::default(),
    });
    let Reply::CommitRecords(committed) = engine.submit(&file, commit).reply else {
        panic!("not op70's reply");
    };
    assert!(matches!(committed.result, Err(CommitError::NotSaved(_))));
    assert_eq!(
        committed.unused,
        Staged::from([(record, staged)]),
        "back to the client"
    );
    assert_eq!(untouched(&file, &disk), before);
    assert_eq!(drawn.load(Ordering::Relaxed), draws, "nothing ran");

    // Reads go on, also the forms of op72 and op87 that only read (the
    // review's P1): a serial read with flags 0, a count with a delta of 0.
    let read = Operation::AdvanceSerial(AdvanceSerial {
        request: GetSerial {
            table: vec![table.clone()],
            field: vec![uint_segment(fixture::COUNT)],
            value: String::new(),
            increment: 1,
            flags: 0,
        },
    });
    let Reply::AdvanceSerial(serial) = engine.submit(&file, read).reply else {
        panic!("not op72's reply");
    };
    assert_eq!((serial.status, serial.value.as_str()), (0, "1"));
    let photo = vec![
        table.clone(),
        vec![0x1f],
        vec![5],
        serial::encode(fixture::PHOTO_MEMBER),
    ];
    let count = |delta| {
        Operation::AdjustContainerCount(AdjustContainerCount {
            request: om_protocol::container::AdjustReferenceCount {
                path: photo.clone(),
                delta,
            },
        })
    };
    assert_eq!(
        engine.submit(&file, count(0)).reply,
        Reply::AdjustContainerCount(1)
    );
    assert_eq!(
        engine.submit(&file, count(1)).reply,
        Reply::AdjustContainerCount(COUNT_FAILED),
        "a change is refused"
    );
    // A program that only reads, with nothing waiting, submits a draft
    // that stores nothing: it is answered, as a read.
    assert_eq!(
        engine.submit_draft(&file, Draft::StoreIndexes).reply,
        Reply::StoreIndexes(Ok(()))
    );
    assert_eq!(
        engine
            .submit_draft(&file, Draft::ChangeSchema { writes: Vec::new() })
            .reply,
        Reply::StoreIndexes(Ok(()))
    );
    assert_eq!(untouched(&file, &disk), before);

    // Lock requests and their releases go on.
    let lock = Operation::TakeLock(TakeLock {
        user,
        holder: "A (Admin)".into(),
        request: Lock {
            waiter: 0,
            timeout: 0,
            want_mod_count: true,
            path: vec![table.clone(), vec![0x05], serial::encode(2)],
            argument: 0,
            host_file: 1,
            txn: user.txn,
            lock_type: LOCK_WRITE,
        },
        room: writes::MAX_LOCKS,
    });
    let taken = engine.submit(&file, lock);
    assert!(matches!(
        taken.reply,
        Reply::TakeLock(LockReply::Granted { .. })
    ));
    let release = Operation::ReleaseLocks(ReleaseLocks {
        connection: user.connection,
        request: KillAllLocks {
            host_file: 1,
            txn: user.txn,
            all: true,
        },
    });
    assert_eq!(engine.submit(&file, release).reply, Reply::ReleaseLocks);
    assert_eq!(file.lock_table().held_by(user.connection), 0);
    assert_eq!(events.alerts().len(), 1, "one alert");

    // Hosted again: as the renamed file holds it, and it takes changes.
    drop((file, hosting));
    let again = load(&disk);
    assert!(again.writes_refused().is_none());
    assert_eq!(again.read_store().last_record_id(&table), 4);
    let outcome = engine.submit(&again, reserve());
    assert!(
        matches!(&outcome.reply, Reply::ReserveRecordIds(r) if r.status == 0 && r.record == [serial::encode(5)]),
        "{:?}",
        outcome.reply
    );
    assert_eq!(last_on_disk(), 5);
}

/// What a draft took of what reads left waiting (the module doc's
/// "Drafts") waits again when its operation is not stored: a find's index
/// and an issued layout key, which an op86 write took, are stored by the
/// next write once saves work.
#[test]
fn what_a_draft_took_waits_again_when_its_operation_is_not_stored() {
    use om_query::{Source as _, Write};
    let mut tree = fixture::tree(&[], true);
    fixture::add_tasks(&mut tree, 1..=3);
    fixture::index_layouts(&mut tree);
    let (disk, file) = host("drafts-not-stored", &tree);
    let table = uint_segment(fixture::TABLE);
    let name = uint_segment(fixture::NAME);
    let issued = file.read_store().generate_member_key(&[vec![4]]).unwrap();
    file.read_store()
        .index_on_demand(&table, &name, 0)
        .unwrap()
        .unwrap();
    let waiting = file.read_store().waiting_for_tests();
    assert_eq!(waiting.0, [(table.clone(), name.clone(), 0)]);
    assert_eq!(waiting.1.len(), 1);
    let before = untouched(&file, &disk);
    let tasks = vec![vec![3], vec![0x10], vec![5], table.clone()];
    let mut renamed = node_at(&file, &tasks).unwrap();
    renamed.set_child(text_node(&[0x10], "Jobs"));
    let draft = || Draft::ChangeSchema {
        writes: vec![Write::Put {
            path: tasks.clone(),
            node: renamed.clone(),
        }],
    };
    let engine = engine();
    file.fail_saves(Some(crate::store::SaveFault::BeforeRename));
    let outcome = engine.submit_draft(&file, draft());
    file.fail_saves(None);
    assert!(is_storage_error(&outcome.reply), "{outcome:?}");
    assert_eq!(
        untouched(&file, &disk),
        before,
        "the index and key wait again"
    );

    // A find's indexes that cannot be stored wait again too.
    file.fail_saves(Some(crate::store::SaveFault::BeforeRename));
    let outcome = engine.submit_draft(&file, Draft::StoreIndexes);
    file.fail_saves(None);
    assert!(is_storage_error(&outcome.reply), "{outcome:?}");
    assert_eq!(untouched(&file, &disk), before);

    let outcome = engine.submit_draft(&file, draft());
    assert_eq!(outcome.reply, Reply::ChangeSchema(Ok(())));
    assert_eq!(node_at(&file, &tasks), Some(renamed));
    let id = node_at(&file, &[vec![4], vec![1], vec![0]]).and_then(|n| n.value);
    assert_eq!(id, Some(issued.0));
    let counts = vec![table.clone(), vec![0x01], name.clone(), vec![0x10]];
    assert!(node_at(&file, &counts).is_some(), "the index is stored");
    assert_eq!(
        file.read_store().waiting_for_tests(),
        (Vec::new(), Vec::new())
    );
}

/// The review's P1s on GenerateMemberKey: request A's keys may be written
/// by its own draft or by request B's write that held the store first. A
/// answers, once it submitted its own draft, from one look at the store
/// (`Store::issued_keys`): stored, its answer stands; the file taking no
/// more changes (B's save's outcome was unknown), unknown; a key its
/// catalog's index does not hold (it waits because no save stored it or
/// the file became read-only, or the write dropped it), the error of a key
/// not stored. Never success for a key not stored or whose save's outcome
/// is unknown.
#[test]
fn a_request_answers_from_what_became_of_the_keys_it_issued() {
    use crate::store::IssuedKeysAre;
    let mut tree = fixture::tree(&[], true);
    fixture::add_tasks(&mut tree, 1..=3);
    let engine = engine();
    let layouts = vec![vec![4]];
    let draft = || Draft::ChangeSchema { writes: Vec::new() };
    let issue = |file: &Hosted| {
        let (id, _) = file.read_store().issue_member_key(&layouts).unwrap();
        vec![(layouts.clone(), id)]
    };
    let ended =
        |file: &Hosted, issued: &[(Vec<Vec<u8>>, u64)]| match file.read_store().issued_keys(issued)
        {
            IssuedKeysAre::Stored => "stored",
            IssuedKeysAre::NotStored => "not stored",
            IssuedKeysAre::Unknown(_) => "unknown",
        };
    // The error A answers (`None`: its program's answer), given its own
    // draft's error and whether it had catalog writes of its own.
    let answered = |file: &Hosted, issued: &[(Vec<Vec<u8>>, u64)], failed, own_writes| {
        file.read_store()
            .issued_keys(issued)
            .error(failed, own_writes)
    };

    // B's write stores A's key before A's own draft, which has nothing left
    // to store: stored. A key C issued since, which waits, is not A's.
    let (_disk, file) = host("keys-stored", &tree);
    let a = issue(&file);
    assert_eq!(
        engine.submit_draft(&file, draft()).reply,
        Reply::ChangeSchema(Ok(()))
    );
    assert_eq!(
        engine.submit_draft(&file, draft()).reply,
        Reply::StoreIndexes(Ok(()))
    );
    let c = issue(&file);
    assert_eq!(ended(&file, &a), "stored");
    assert_eq!(answered(&file, &a, None, false), None);
    assert_eq!(ended(&file, &c), "not stored");
    assert_eq!(answered(&file, &c, None, false), Some(800));

    // B's save stores nothing: A's key waits again, and A's own draft
    // stores it.
    let (_disk, file) = host("keys-not-stored", &tree);
    let a = issue(&file);
    file.fail_saves(Some(crate::store::SaveFault::BeforeRename));
    let b = engine.submit_draft(&file, draft());
    file.fail_saves(None);
    assert!(is_storage_error(&b.reply), "{b:?}");
    assert_eq!(ended(&file, &a), "not stored");
    assert_eq!(
        engine.submit_draft(&file, draft()).reply,
        Reply::ChangeSchema(Ok(()))
    );
    assert_eq!(ended(&file, &a), "stored");

    // A's own save stores nothing: its key waits, and A answers the
    // save's error. B's write then stores the key before A looks: A, whose
    // only write is its key, answers as stored; a program whose own
    // catalog writes failed with it keeps that failure.
    let a = issue(&file);
    file.fail_saves(Some(crate::store::SaveFault::BeforeRename));
    let own = engine.submit_draft(&file, draft());
    file.fail_saves(None);
    assert!(is_storage_error(&own.reply), "{own:?}");
    assert_eq!(ended(&file, &a), "not stored");
    assert_eq!(answered(&file, &a, Some(800), false), Some(800));
    assert_eq!(
        engine.submit_draft(&file, draft()).reply,
        Reply::ChangeSchema(Ok(()))
    );
    assert_eq!(answered(&file, &a, Some(800), false), None, "its answer");
    assert_eq!(
        answered(&file, &a, Some(809), true),
        Some(809),
        "its writes'"
    );

    // The file becomes read-only after A issued its key: A's draft takes
    // nothing and answers as a read; the key waits.
    let (_disk, file) = host("keys-read-only", &tree);
    let a = issue(&file);
    file.write_store(&super::WRITER)
        .set_read_only(crate::store::ReadOnlyCause::DeferredWork);
    assert_eq!(
        engine.submit_draft(&file, draft()).reply,
        Reply::StoreIndexes(Ok(()))
    );
    assert_eq!(ended(&file, &a), "not stored");

    // B's draft writes A's key into the field catalog of a table and then
    // deletes the table, the counter with it: the key is in no index
    // (the counter a draft's own writes remove, #736).
    let (_disk, file) = host("keys-dropped", &tree);
    let table = uint_segment(fixture::TABLE);
    let fields = vec![table.clone(), vec![3]];
    let (id, _) = file.read_store().issue_member_key(&fields).unwrap();
    let a = vec![(fields.clone(), id)];
    let delete = Draft::ChangeSchema {
        writes: vec![om_query::Write::Remove { path: vec![table] }],
    };
    assert_eq!(
        engine.submit_draft(&file, delete).reply,
        Reply::ChangeSchema(Ok(()))
    );
    assert_eq!(file.read_store().waiting_for_tests().1, Vec::new());
    assert_eq!(ended(&file, &a), "not stored");

    // B's save's outcome is unknown: A is told so, though nothing of its
    // own is left to store.
    let (_disk, file) = host("keys-unknown", &tree);
    let a = issue(&file);
    file.fail_saves(Some(crate::store::SaveFault::FolderFlush));
    let b = engine.submit_draft(&file, draft());
    file.fail_saves(None);
    assert!(
        matches!(&b.reply, Reply::ChangeSchema(Err(SchemaNotChanged::NotStored(e))) if e.unknown),
        "{b:?}"
    );
    assert_eq!(ended(&file, &a), "unknown");
    assert_eq!(answered(&file, &a, None, false), Some(800));
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
    assert_eq!(outcome.reply, Reply::UploadToCatalog(Ok(true)));
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

/// op73 recomputes a stored calculation whose formula op13 changed, and
/// takes its basis from the field definitions as they are now: when its
/// save fails, the records, the work list and the basis are put back, so
/// that op73 submitted again recomputes the same values (a basis left
/// changed would find nothing stale).
#[test]
fn a_table_update_that_is_not_stored_keeps_its_basis() {
    let mut tree = fixture::tree(&[], true);
    fixture::add_tasks(&mut tree, 1..=3);
    let table = uint_segment(fixture::TABLE);
    let (_recorded, file) = host("engine-formula-failing", &tree);
    let mut catalog = tree.at(&[&table, &[3]]).unwrap().clone();
    set_formula(&mut catalog, fixture::DOUBLED, "Count * 3");
    catalog.put(
        &[&[1], &[0xfd], &uint_segment(fixture::DOUBLED)],
        OwnedNode::leaf(Vec::new(), 0xc0u32.to_be_bytes().to_vec()),
    );
    let steps: Vec<(Operation, Given, Outcome)> = [
        Operation::UploadToCatalog(UploadToCatalog {
            path: vec![table.clone(), vec![3]],
            node: catalog,
        }),
        Operation::UpdateTable(UpdateTable {
            table: table.clone(),
            account: fixture::ACCOUNT.into(),
        }),
    ]
    .into_iter()
    .map(|operation| {
        let outcome = engine().submit(&file, operation.clone());
        (operation, Given::Nothing, outcome)
    })
    .collect();
    assert_eq!(steps[1].2.reply, Reply::UpdateTable(Ok(true)));
    let doubled = |record: u64| {
        let path = vec![
            table.clone(),
            vec![0x05],
            serial::encode(record),
            uint_segment(fixture::DOUBLED),
        ];
        node_at(&file, &path).and_then(|n| n.value)
    };
    assert_eq!(doubled(2), text_node(&[], "6").value);
    let last = file.read_store().node(&[]).unwrap().clone();
    let taken_back =
        each_failed_save_takes_its_step_back("engine-formula-failing", &tree, &steps, &last);
    assert_eq!(taken_back, [0, 1]);
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

/// A fixed clock, a random source that gives `byte` for every byte, and
/// virtual stopwatches advancing by `step`, each from its own start
/// ([`virtual_stopwatch`]); `readings` counts the readings of all of them.
struct Fixed {
    now: &'static str,
    byte: u8,
    step: Duration,
    readings: Arc<AtomicU64>,
}

impl Fixed {
    fn new(now: &'static str, byte: u8, step: Duration) -> Self {
        Fixed {
            now,
            byte,
            step,
            readings: Arc::default(),
        }
    }
}

/// The account catalog's index as FileMaker keeps it (local-files.md 3)
/// for the accounts [`fixture::add_local_accounts`] gives: its name list,
/// display order, `07` entries and count, which Delete Account rewrites
/// and every stamped account edit stamps.
fn index_accounts(tree: &mut OwnedNode) {
    let leaf = |value: Vec<u8>| OwnedNode::leaf(Vec::new(), value);
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
    index_accounts(&mut tree);
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
        user: fixture::ACCOUNT.into(),
        account: fixture::ACCOUNT.into(),
    };
    // An account edit tells no one: a server script tells the other
    // clients when it releases its lock on the catalog
    // (`ReleaseScriptLock`; `scripts::tests`).
    let nothing_told = Notices::default();
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
    assert_eq!(outcome.notices, nothing_told);
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
    // too.
    let reset = |account: &str| {
        Operation::ResetPassword(ResetPassword {
            account: account.into(),
            expire: false,
            stamp: Some(stamp.clone()),
        })
    };
    let outcome = submit(reset(fixture::CLERK), Given::Reset(CLERK_NEW), true);
    assert_eq!(outcome.reply, Reply::ResetPassword(Ok(())));
    assert_eq!(outcome.notices, nothing_told);
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
    assert_eq!(outcome.notices, nothing_told);
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
    assert_eq!(outcome.notices, nothing_told);
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
    assert_eq!(outcome.reply, Reply::UploadToCatalog(Ok(true)));
    let outcome = submit(Operation::FinishDeferredWork, Given::Nothing, true);
    assert_eq!(outcome.reply, Reply::FinishDeferredWork(Ok(true)));
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
    let steps: Vec<_> = recorded
        .iter()
        .map(|(operation, given, outcome, _, _)| (operation.clone(), *given, outcome.clone()))
        .collect();
    let last = recorded.last().unwrap().3.clone();
    let taken_back =
        each_failed_save_takes_its_step_back("engine-accounts-failing", &tree, &steps, &last);
    // Each account edit that changed the file, op13, the deferred work and
    // the found set; the refusals and the edit that changed nothing save
    // nothing.
    assert_eq!(taken_back, [0, 1, 3, 4, 6, 8, 9, 10]);

    // A file opened with that work pending has it finished, and saved, by
    // the engine that loads it.
    let mut pending = tree.clone();
    let path: Vec<&[u8]> = deferred.iter().map(Vec::as_slice).collect();
    pending.put(&path, work);
    let saved = fixture::TempFile::from_tree("engine-deferred-at-load", &pending);
    let (loading, log) = recording();
    let file = load_with(&saved, &loading);
    assert_eq!(*log.lock().unwrap(), [Operation::FinishDeferredWork]);
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

    fn stopwatch(&self) -> om_data::rules::Stopwatch {
        virtual_stopwatch(Duration::from_millis(1), Arc::default())
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

impl Environment for Fixed {
    fn now(&self) -> jiff::Zoned {
        self.now.parse().unwrap()
    }

    fn fill_random(&self, bytes: &mut [u8]) {
        bytes.fill(self.byte);
    }

    fn stopwatch(&self) -> om_data::rules::Stopwatch {
        virtual_stopwatch(self.step, Arc::clone(&self.readings))
    }
}

/// An op70 by Admin editing record 1 of the fixture's Tasks under a custom
/// set whose edit limit is `limit`, compiled in the Tasks context.
fn limited_edit(limit: &str) -> Operation {
    use om_data::host::privilege_set::{
        AccessLevel, Actions, Allow, CustomRules, MenuCommands, TableRules,
    };
    let names = fixture::names();
    let mut context = om_formula::Context::new(&names);
    context.occurrence = Some(fixture::OCCURRENCE);
    let limit = om_formula::compile(limit, &context).unwrap();
    let rules = TableRules {
        actions: Actions {
            create: Allow::Yes,
            view: Allow::Yes,
            edit: Allow::Limited(om_formula::polish::encode(&limit.expr)),
            delete: Allow::Yes,
        },
        fields: AccessLevel::Modifiable,
        field_levels: Default::default(),
    };
    let access = SetAccess::Custom(Box::new(CustomRules {
        default: rules,
        tables: Default::default(),
        layouts: Default::default(),
        scripts: Default::default(),
        value_lists: Default::default(),
        prints: true,
        exports: true,
        menus: MenuCommands::All,
    }));
    let table = uint_segment(fixture::TABLE);
    let record = vec![table.clone(), vec![0x05], serial::encode(1)];
    let edited = OwnedNode::list(
        serial::encode(1),
        vec![
            text_node(&uint_segment(fixture::NAME), "Task one"),
            serial_leaf(MOD_COUNT, 2),
        ],
    );
    let entry = OwnedNode::list(
        serial::encode(1),
        vec![OwnedNode::leaf(uint_segment(fixture::NAME), Vec::new())],
    );
    Operation::CommitRecords(CommitRecords {
        user: LockUser {
            connection: 1,
            txn: 7,
        },
        account: fixture::ACCOUNT.into(),
        access,
        list: OwnedNode::list(
            Vec::new(),
            vec![OwnedNode::list(
                table,
                vec![OwnedNode::list(vec![0x05], vec![entry])],
            )],
        ),
        staged: Staged::from([(record, edited)]),
        withheld: Withheld::default(),
    })
}

/// `operation` submitted with `environment` on a fresh load of a file of
/// one task: its outcome and the tree it leaves.
fn submit_fresh(operation: &Operation, environment: Fixed) -> (Outcome, Option<OwnedNode>) {
    let mut tree = fixture::tree(&[], true);
    fixture::add_tasks(&mut tree, 1..=1);
    let file = fixture::TempFile::from_tree("engine-limit", &tree);
    let hosted = HostedFile {
        name: fixture::FILE.into(),
        path: file.0.clone(),
    };
    let engine = FileEngine::new(Box::new(environment));
    let hosting = Hosting::load(&[hosted], false, &EventLog::default(), &engine).unwrap();
    let hosted = hosting.by_name(fixture::FILE).unwrap();
    let outcome = engine.submit(&hosted, operation.clone());
    let stored = hosted.read_store().node(&[]).cloned();
    (outcome, stored)
}

fn allowed(outcome: &Outcome) -> bool {
    match &outcome.reply {
        Reply::CommitRecords(CommitReply { result: Ok(()), .. }) => true,
        Reply::CommitRecords(CommitReply {
            result: Err(CommitError::Refused(_)),
            ..
        }) => false,
        other => panic!("{other:?}"),
    }
}

/// #692: the formulas an operation evaluates take the date, `Random` and
/// their time budget from the engine's environment. An op70 whose record
/// limit reads the date (as `Get` answers it, and as a date without a year
/// takes it) and two `Random` draws, in a year no system clock reads,
/// replays to the same outcome under the same environment. It is refused
/// under another date, under another seed, and under a stopwatch whose
/// step meets the budget at the loop's first check. The draws are compared
/// with the exact sequence the seed gives, which no other generator gives
/// but by a chance of one in 10^12.
#[test]
fn a_record_limit_takes_the_date_random_and_its_budget_from_the_environment() {
    // The engine seeds the operation's generator with eight bytes of the
    // environment's random source.
    let draws = |byte: u8| {
        let mut generator = om_calc::eval::Lcg(u64::from_le_bytes([byte; 8]));
        [(); 2].map(|()| (generator.next_f64() * 1_000_000.0).floor() as u64)
    };
    let (seed, other) = (7, 1);
    assert_ne!(draws(seed), draws(other));
    let [first, second] = draws(seed);
    let operation = limited_edit(&format!(
        "While ( [ i = 0 ] ; i < 100 ; [ i = i + 1 ] ; i ) = 100 \
         and Get ( CurrentDate ) = Date ( 11 ; 24 ; 2999 ) \
         and GetAsDate ( \"11/24\" ) = Get ( CurrentDate ) \
         and Floor ( Random * 1000000 ) = {first} and Floor ( Random * 1000000 ) = {second}"
    ));
    let today = "2999-11-24T12:00:00[UTC]";
    let step = Duration::from_millis(1);
    let (recorded, tree) = submit_fresh(&operation, Fixed::new(today, seed, step));
    assert!(allowed(&recorded), "{:?}", recorded.reply);
    assert_eq!(
        submit_fresh(&operation, Fixed::new(today, seed, step)),
        (recorded, tree)
    );
    for environment in [
        Fixed::new("2999-11-25T12:00:00[UTC]", seed, step),
        Fixed::new(today, other, step),
        Fixed::new(today, seed, Duration::from_secs(1)),
    ] {
        let (now, byte, step) = (environment.now, environment.byte, environment.step);
        let (outcome, _) = submit_fresh(&operation, environment);
        assert!(!allowed(&outcome), "{now} {byte} {step:?}");
    }
}

/// A record limit that would loop for ever under a large `SetRecursion`
/// allowance is refused once the seeded stopwatch reaches the 100 ms
/// budget: after the same number of the evaluator's checks on every run,
/// one reading to start and one per check, the hundredth at 100 ms.
#[test]
fn a_runaway_record_limit_is_refused_after_the_same_number_of_checks() {
    let operation = limited_edit(
        "SetRecursion ( While ( [ i = 0 ] ; 1 ; [ i = i + 1 ] ; i ) ; 10000000000000000000 ) > 0",
    );
    let readings: Vec<u64> = (0..2)
        .map(|_| {
            let environment = Fixed::new("2999-11-24T12:00:00[UTC]", 7, Duration::from_millis(1));
            let readings = Arc::clone(&environment.readings);
            let (outcome, _) = submit_fresh(&operation, environment);
            assert!(!allowed(&outcome), "{:?}", outcome.reply);
            readings.load(Ordering::Relaxed)
        })
        .collect();
    assert_eq!(readings, [101, 101]);
}

/// A supplied clock at the ends of FileMaker's dates: a reading just before
/// 1970 keeps its UTC day for a date without a year (the instant is
/// floored, not truncated toward zero), and one past 12/31/4000 answers the
/// clock selectors with error 500, as an out-of-range date is
/// (calc-values.md 4.1), never with the system clock.
#[test]
fn a_supplied_clock_at_the_ends_of_filemakers_dates_reads_no_other_clock() {
    let step = Duration::from_millis(1);
    let before_1970 = "1969-12-31T23:59:59.999999999[UTC]";
    let environment =
        crate::recalc::record_environment_in("Admin", &Fixed::new(before_1970, 7, step));
    assert_eq!(environment.date, "12/31/1969");
    assert_eq!(
        environment.sources.map(|sources| sources.utc_micros),
        Some(-1)
    );
    let operation = limited_edit(
        "Get ( CurrentDate ) = Date ( 12 ; 31 ; 1969 ) and GetAsDate ( \"12/31\" ) = Get ( CurrentDate )",
    );
    let (outcome, _) = submit_fresh(&operation, Fixed::new(before_1970, 7, step));
    assert!(allowed(&outcome), "{:?}", outcome.reply);
    let operation = limited_edit(
        "EvaluationError ( Get ( CurrentDate ) ) = 500 and EvaluationError ( Get ( CurrentTime ) ) = 500 \
         and EvaluationError ( Get ( CurrentTimestamp ) ) = 500",
    );
    let (outcome, _) = submit_fresh(&operation, Fixed::new("4001-01-01T12:00:00[UTC]", 7, step));
    assert!(allowed(&outcome), "{:?}", outcome.reply);
}

/// A draw an operation takes from its environment ([`Counting`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Draw {
    Clock,
    /// Bytes of the random source, by `fill_random` or `try_fill_random`.
    Random(usize),
    Stopwatch,
}

/// [`Seeded`], noting each draw an operation takes from it, in order.
struct Counting {
    seeded: Seeded,
    draws: Arc<Mutex<Vec<Draw>>>,
}

impl Environment for Counting {
    fn now(&self) -> jiff::Zoned {
        self.draws.lock().unwrap().push(Draw::Clock);
        self.seeded.now()
    }

    fn fill_random(&self, bytes: &mut [u8]) {
        self.draws.lock().unwrap().push(Draw::Random(bytes.len()));
        self.seeded.fill_random(bytes);
    }

    fn stopwatch(&self) -> om_data::rules::Stopwatch {
        self.draws.lock().unwrap().push(Draw::Stopwatch);
        self.seeded.stopwatch()
    }
}

/// The engine's module doc, "The environment", says in what order each
/// operation draws from its environment, so that a seeded replay draws
/// the same; this checks one operation of each kind against that list, on
/// a fresh counting environment each: op70; op73 and the deferred work
/// finished at opening; op88 into a new library, then the same data again;
/// a server script's commit with container data into a new library; an
/// authoring edit with no UUID and one with two; Change Password done,
/// refused before the salt and refused after it (a stored checksum the
/// account's members do not give); Reset Account Password; Enable Account
/// changing the account and changing nothing; Delete Account; and
/// operations that draw nothing.
#[test]
fn each_kind_of_operation_draws_from_its_environment_as_written_here() {
    use om_protocol::authoring::{self as design, catalog_revision, content_revision};
    use Draw::{Clock, Random, Stopwatch};

    use crate::writes::RecordWrite;
    // The draws of a record environment (`recalc::record_environment_in`).
    const RECORD_ENVIRONMENT: [Draw; 4] = [Random(16), Clock, Random(8), Stopwatch];
    // Container data that becomes the first member of a new library: the
    // library's unique id, then the member's (`library::store`).
    const NEW_LIBRARY: [Draw; 2] = [Random(16), Random(16)];
    const SCRIPT: u64 = 5;
    let table = uint_segment(fixture::TABLE);
    let script = fixture::script(SCRIPT, "Greeting", vec![fixture::exit_with("1")]);
    let mut tree = fixture::tree(std::slice::from_ref(&script), true);
    fixture::index_scripts(&mut tree);
    fixture::add_tasks(&mut tree, 1..=3);
    fixture::add_local_accounts(&mut tree);
    index_accounts(&mut tree);
    index_fields(&mut tree, fixture::TABLE);
    let drawn = |file: &Hosted, operation: Operation, given: Given| {
        let draws = Arc::new(Mutex::new(Vec::new()));
        let environment = Counting {
            seeded: Seeded(AtomicU64::new(0)),
            draws: Arc::clone(&draws),
        };
        let outcome = FileEngine::new(Box::new(environment)).submit_with(
            file,
            operation,
            given.credentials(),
        );
        let drawn = draws.lock().unwrap().clone();
        (outcome, drawn)
    };
    let (_file, file) = host("engine-draws", &tree);
    let stamp = AccountStamp {
        user: fixture::ACCOUNT.into(),
        account: fixture::ACCOUNT.into(),
    };

    // op70: one record environment.
    let (outcome, draws) = drawn(&file, limited_edit("1"), Given::Nothing);
    assert!(matches!(
        outcome.reply,
        Reply::CommitRecords(CommitReply { result: Ok(()), .. })
    ));
    assert_eq!(draws, RECORD_ENVIRONMENT);

    // op73: Count's type changed, so Doubled is recomputed over the table,
    // whose calculations do not read `Get ( UUID )`: one record
    // environment. The upload draws nothing.
    let deferred = vec![table.clone(), vec![0x03], vec![0x01], vec![0xfd]];
    let work = OwnedNode::list(
        vec![0xfd],
        vec![OwnedNode::leaf(
            uint_segment(fixture::COUNT),
            0x40u32.to_be_bytes().to_vec(),
        )],
    );
    let upload = |work: &OwnedNode| {
        Operation::UploadToCatalog(UploadToCatalog {
            path: deferred.clone(),
            node: work.clone(),
        })
    };
    assert_eq!(drawn(&file, upload(&work), Given::Nothing).1, []);
    let update = Operation::UpdateTable(UpdateTable {
        table: table.clone(),
        account: fixture::ACCOUNT.into(),
    });
    let (outcome, draws) = drawn(&file, update, Given::Nothing);
    assert_eq!(outcome.reply, Reply::UpdateTable(Ok(true)));
    assert_eq!(draws, RECORD_ENVIRONMENT);
    // The same work, finished as at opening.
    drawn(&file, upload(&work), Given::Nothing);
    let (outcome, draws) = drawn(&file, Operation::FinishDeferredWork, Given::Nothing);
    assert_eq!(outcome.reply, Reply::FinishDeferredWork(Ok(true)));
    assert_eq!(draws, RECORD_ENVIRONMENT);

    // op88 into Notes' library, which is new; the same data again is the
    // member the library holds.
    let store = |bytes: &[u8]| {
        Operation::StoreContainerData(StoreContainerData {
            table: vec![uint_segment(fixture::NOTES)],
            data: Ok(file_data(bytes)),
            bytes: 64,
        })
    };
    let (outcome, draws) = drawn(&file, store(b"a note"), Given::Nothing);
    assert!(matches!(outcome.reply, Reply::StoreContainerData(r) if r.status == 0));
    assert_eq!(draws, NEW_LIBRARY);
    assert_eq!(drawn(&file, store(b"a note"), Given::Nothing).1, []);
    let (_, draws) = drawn(&file, store(b"another note"), Given::Nothing);
    assert_eq!(draws, [Random(16)], "a new member of a library that exists");

    // A server script's commit of record 2 with a photo, into Tasks'
    // library, which is new.
    let mut edited = node_at(&file, &[table.clone(), vec![0x05], serial::encode(2)]).unwrap();
    edited.set_child(serial_leaf(MOD_COUNT, 1));
    let commit = Operation::CommitScriptRecords(CommitScriptRecords {
        user: LockUser {
            connection: 9,
            txn: 1,
        },
        records: vec![RecordWrite {
            table: fixture::TABLE,
            id: 2,
            record: Some(edited),
            new: false,
            uploads: vec![(fixture::PHOTO, file_data(b"a photo"))],
        }],
        bulk_delete: false,
    });
    let (outcome, draws) = drawn(&file, commit, Given::Nothing);
    assert_eq!(outcome.reply, Reply::CommitScriptRecords(Ok(())));
    assert_eq!(draws, NEW_LIBRARY);

    // Authoring edits: a script's steps (no UUID), a new field (two).
    let edit = |edit: DesignEdit| {
        Operation::EditDesign(Box::new(EditDesign {
            connection: 3,
            session_name: "Designer".into(),
            account: fixture::ACCOUNT.into(),
            edit,
        }))
    };
    let definition = [vec![17], vec![5], serial::encode(SCRIPT)];
    let path: Vec<&[u8]> = definition.iter().map(Vec::as_slice).collect();
    let current = file.read_store().node(&path).cloned().unwrap();
    let steps = fixture::script(SCRIPT, "Greeting", vec![fixture::exit_with("2")]).encode();
    let replace = design::ReplaceScriptSteps {
        script: SCRIPT,
        expected: content_revision(&current),
        definition: steps,
    };
    let (_, draws) = drawn(
        &file,
        edit(DesignEdit::ReplaceScriptSteps(replace)),
        Given::Nothing,
    );
    assert_eq!(draws, [Clock]);
    let fields = file.read_store().node(&[&table, &[3]]).cloned().unwrap();
    let create = design::CreateField {
        table: fixture::TABLE,
        expected: catalog_revision(&fields),
        data_type: 1,
        name: "Priority".into(),
        comment: String::new(),
    };
    let (_, draws) = drawn(&file, edit(DesignEdit::CreateField(create)), Given::Nothing);
    assert_eq!(draws, [Clock, Random(16), Random(16)]);

    // Change Password: done; refused for a wrong old password, before the
    // salt; refused for a stored checksum its members do not give, after
    // it.
    let change = |account: &str| {
        Operation::ChangePassword(ChangePassword {
            account: AccountRef::Name(account.into()),
            manager: false,
            stamp: None,
        })
    };
    let given = Given::Change(fixture::PASSWORD, "a new horse");
    let (outcome, draws) = drawn(&file, change(fixture::ACCOUNT), given);
    assert_eq!(outcome.reply, Reply::ChangePassword(Ok(())));
    assert_eq!(draws, [Clock, Random(10)]);
    let given = Given::Change("not it", "a new horse");
    let (outcome, draws) = drawn(&file, change(fixture::ACCOUNT), given);
    assert_eq!(
        outcome.reply,
        Reply::ChangePassword(Err(PasswordChangeError::Refused))
    );
    assert_eq!(draws, [Clock]);
    let mut bad_sum = tree.clone();
    bad_sum.put(
        &[&[23], &[1], &[5], &[5], &[4]],
        OwnedNode::leaf(Vec::new(), vec![0; 20]),
    );
    let (_bad, bad) = host("engine-draws-checksum", &bad_sum);
    let given = Given::Change(fixture::NEWCOMER_PASSWORD, "settled in");
    let (outcome, draws) = drawn(&bad, change(fixture::NEWCOMER), given);
    assert!(matches!(
        outcome.reply,
        Reply::ChangePassword(Err(PasswordChangeError::Unwritten(_)))
    ));
    assert_eq!(draws, [Clock, Random(10)], "the checksum is checked last");

    // Reset Account Password, Enable Account (a change, then none) and
    // Delete Account, stamped as a script's are.
    let reset = Operation::ResetPassword(ResetPassword {
        account: fixture::CLERK.into(),
        expire: false,
        stamp: Some(stamp.clone()),
    });
    let (outcome, draws) = drawn(&file, reset, Given::Reset("fresh"));
    assert_eq!(outcome.reply, Reply::ResetPassword(Ok(())));
    assert_eq!(draws, [Clock, Random(10)]);
    let enable = Operation::EnableAccount(EnableAccount {
        account: fixture::FORMER.into(),
        active: true,
        stamp: Some(stamp.clone()),
    });
    let (outcome, draws) = drawn(&file, enable.clone(), Given::Nothing);
    assert_eq!(outcome.reply, Reply::EnableAccount(Ok(true)));
    assert_eq!(draws, [Clock]);
    let (outcome, draws) = drawn(&file, enable, Given::Nothing);
    assert_eq!(outcome.reply, Reply::EnableAccount(Ok(false)));
    assert_eq!(draws, []);
    let delete = Operation::DeleteAccount(DeleteAccount {
        account: fixture::CLERK.into(),
        stamp: Some(stamp),
    });
    let (outcome, draws) = drawn(&file, delete, Given::Nothing);
    assert_eq!(outcome.reply, Reply::DeleteAccount(Ok(())));
    assert_eq!(draws, [Clock]);

    // Operations that draw nothing.
    let serial = Operation::AdvanceSerial(AdvanceSerial {
        request: GetSerial {
            table: vec![table.clone()],
            field: vec![uint_segment(fixture::COUNT)],
            value: String::new(),
            increment: 1,
            flags: SERIAL_INCREMENT,
        },
    });
    let reserve = Operation::ReserveRecordIds(ReserveRecordIds {
        user: LockUser {
            connection: 1,
            txn: 7,
        },
        holder: "A (Admin)".into(),
        request: CreateRecord {
            table: vec![table.clone()],
            count: 1,
            txn: 7,
        },
        room: writes::MAX_LOCKS,
        durable: true,
    });
    for operation in [serial, reserve, Operation::SaveFile] {
        let (_, draws) = drawn(&file, operation.clone(), Given::Nothing);
        assert_eq!(draws, [], "{operation:?}");
    }
}

/// Each stopwatch a test environment gives measures from its own start
/// ([`virtual_stopwatch`]), whatever others were read, so that one
/// operation's time budget does not depend on another's readings.
#[test]
fn each_virtual_stopwatch_measures_from_its_own_start() {
    let environment = Fixed::new("2026-11-24T12:00:00[UTC]", 7, Duration::from_millis(1));
    let first = environment.stopwatch();
    assert_eq!(
        (first(), first()),
        (Duration::ZERO, Duration::from_millis(1))
    );
    let second = environment.stopwatch();
    assert_eq!(second(), Duration::ZERO);
    assert_eq!(first(), Duration::from_millis(2));
    assert_eq!(environment.readings.load(Ordering::Relaxed), 4);
}

/// Each step of `operations`, submitted in order to a fresh load of `tree`
/// by [`engine`], with its outcome, and the tree they left: a recording for
/// [`each_failed_save_takes_its_step_back`].
fn recorded(
    tag: &str,
    tree: &OwnedNode,
    operations: Vec<Operation>,
) -> (Vec<(Operation, Given, Outcome)>, OwnedNode) {
    let (_file, file) = host(tag, tree);
    let engine = engine();
    let steps = operations
        .into_iter()
        .map(|operation| {
            let outcome = engine.submit(&file, operation.clone());
            (operation, Given::Nothing, outcome)
        })
        .collect();
    let last = file.read_store().node(&[]).unwrap().clone();
    (steps, last)
}

/// docs/server-storage.md 5: taking a change back restores what did not
/// exist as well as what did. On a file whose Notes has no record yet (no
/// record list `[T, 05]`, record-id list or last issued id), whose Tasks has
/// no library and no index, and whose layout catalog has no index `[4,
/// 01]`, each step creates the node it writes into; each is taken back
/// when its save fails, and nothing it created is left behind: the first
/// op71 and op70 of a table (the case of the review's P1 on the first
/// record), op72's first serial value, op88's first library member and
/// op87's count, a find's first index, op86's first member key of a
/// catalog and its write under a parent that did not exist, op13's upload
/// under one, and a found set at an occurrence with nothing kept yet.
#[test]
fn a_change_whose_save_fails_takes_back_the_nodes_it_created() {
    let mut tree = fixture::tree(&[], true);
    let notes = uint_segment(fixture::NOTES);
    let tasks = uint_segment(fixture::TABLE);
    for key in [0x05, 0x0d, 0x00] {
        tree.remove(&[&notes, &[key]]);
    }
    for key in [0x01, 0x1f] {
        assert!(tree.at(&[&tasks, &[key]]).is_none());
    }
    assert!(tree.at(&[&[4], &[1]]).is_none());
    tree.put(
        &[&[3], &[0x10], &[5], &tasks, &[0xfb]],
        OwnedNode::leaf(Vec::new(), 7u32.to_be_bytes().to_vec()),
    );
    let (_scratch, scratch) = host("engine-bare-access", &tree);
    let access = scratch
        .account(fixture::ACCOUNT)
        .unwrap()
        .access
        .clone()
        .unwrap();
    let user = LockUser {
        connection: 1,
        txn: 7,
    };
    let record = vec![notes.clone(), vec![0x05], serial::encode(1)];
    let staged = OwnedNode::list(
        serial::encode(1),
        vec![
            text_node(&uint_segment(fixture::BODY), "The first note"),
            serial_leaf(MOD_COUNT, 1),
        ],
    );
    let list = OwnedNode::list(
        Vec::new(),
        vec![OwnedNode::list(
            notes.clone(),
            vec![OwnedNode::list(
                vec![0x05],
                vec![OwnedNode::leaf(serial::encode(1), vec![COMMIT_NEW])],
            )],
        )],
    );
    let library = vec![tasks.clone(), vec![0x1f]];
    let member = vec![tasks.clone(), vec![0x1f], vec![5], serial::encode(1)];
    let operations = vec![
        Operation::ReserveRecordIds(ReserveRecordIds {
            user,
            holder: "A (Admin)".into(),
            request: CreateRecord {
                table: vec![notes.clone()],
                count: 1,
                txn: user.txn,
            },
            room: writes::MAX_LOCKS,
            durable: true,
        }),
        Operation::CommitRecords(CommitRecords {
            user,
            account: fixture::ACCOUNT.into(),
            access,
            list,
            staged: Staged::from([(record, staged)]),
            withheld: Withheld::default(),
        }),
        Operation::AdvanceSerial(AdvanceSerial {
            request: GetSerial {
                table: vec![tasks.clone()],
                field: vec![uint_segment(fixture::COUNT)],
                value: String::new(),
                increment: 1,
                flags: SERIAL_INCREMENT,
            },
        }),
        Operation::StoreContainerData(StoreContainerData {
            table: vec![tasks.clone()],
            data: Ok(file_data(b"the first member")),
            bytes: 16,
        }),
        Operation::AdjustContainerCount(AdjustContainerCount {
            request: om_protocol::container::AdjustReferenceCount {
                path: member,
                delta: 1,
            },
        }),
        Operation::StoreIndexes(StoreIndexes {
            indexes: vec![(tasks.clone(), uint_segment(fixture::NAME), 0)],
        }),
        Operation::ChangeSchema(ChangeSchema {
            writes: vec![om_query::Write::Put {
                path: vec![vec![0x30], vec![1], vec![2]],
                node: text_node(&[], "under a new parent"),
            }],
            member_keys: vec![(vec![vec![4]], (5, 9))],
            indexes: Vec::new(),
        }),
        Operation::UploadToCatalog(UploadToCatalog {
            path: vec![vec![0x31], vec![1]],
            node: OwnedNode::list(Vec::new(), vec![text_node(&[2], "uploaded")]),
        }),
        Operation::SaveFoundSet(SaveFoundSet {
            occurrence: uint_segment(fixture::OCCURRENCE),
            table: tasks.clone(),
            records: Some(RidList::from_iter([1])),
            current: Some(1),
        }),
    ];
    let (steps, last) = recorded("engine-bare-recorded", &tree, operations);
    fn path(p: &[Vec<u8>]) -> Vec<&[u8]> {
        p.iter().map(Vec::as_slice).collect()
    }
    for (created, what) in [
        (vec![notes.clone(), vec![0x05]], "Notes' record list"),
        (vec![notes.clone(), vec![0x0d]], "Notes' record-id list"),
        (vec![notes.clone(), vec![0x00]], "Notes' last issued id"),
        (library.clone(), "Tasks' library"),
        (vec![tasks.clone(), vec![0x01]], "Tasks' indexes"),
        (vec![vec![4], vec![1]], "the layout catalog's index"),
        (vec![vec![0x30]], "op86's new parent"),
        (vec![vec![0x31]], "op13's new parent"),
    ] {
        assert!(last.at(&path(&created)).is_some(), "{what} created");
    }
    let taken_back = each_failed_save_takes_its_step_back("engine-bare", &tree, &steps, &last);
    assert_eq!(taken_back, (0..steps.len()).collect::<Vec<_>>());
}

/// The review's P1 on op88: a library whose MD5 index names, for the
/// uploaded data, a member under a key that is not a serial key, which a
/// store does not reuse. The store adds a new member and changes the
/// header, and the journal keeps those, from the same decision
/// (`library::store_plan`): a save that fails leaves the library as it
/// was.
#[test]
fn an_upload_a_library_names_under_a_key_it_does_not_reuse_is_taken_back() {
    let mut tree = fixture::tree(&[], true);
    fixture::add_photo(&mut tree);
    let tasks = uint_segment(fixture::TABLE);
    let data = file_data(b"named under another key");
    tree.put(
        &[&tasks, &[0x1f], &[1], &[8], &data.md5],
        OwnedNode::list(Vec::new(), vec![OwnedNode::leaf(vec![0xff], Vec::new())]),
    );
    tree.put(
        &[&tasks, &[0x1f], &[5], &[0xff]],
        OwnedNode::list(
            Vec::new(),
            vec![OwnedNode::leaf(vec![0x12], 1u32.to_be_bytes().to_vec())],
        ),
    );
    let operations = vec![Operation::StoreContainerData(StoreContainerData {
        table: vec![tasks.clone()],
        data: Ok(data),
        bytes: 23,
    })];
    let (steps, last) = recorded("engine-non-serial-recorded", &tree, operations);
    let Reply::StoreContainerData(stored) = &steps[0].2.reply else {
        panic!("not op88's reply");
    };
    assert_eq!((stored.status, stored.member.len()), (0, 1), "a new member");
    let taken_back =
        each_failed_save_takes_its_step_back("engine-non-serial", &tree, &steps, &last);
    assert_eq!(taken_back, [0]);
}

/// The file at `file` hosted for the desktop, its deferred work not yet
/// finished (that waits for the first admitted login, `Hosted::recover`).
fn local(file: &fixture::TempFile) -> Arc<Hosted> {
    let hosted = HostedFile {
        name: fixture::FILE.into(),
        path: file.0.clone(),
    };
    let hbam = om_hbam::HbamFile::open(&file.0).unwrap();
    let hosting = Hosting::local(&hosted, &hbam, None, false).unwrap();
    hosting.by_name(fixture::FILE).unwrap()
}

/// Tasks with deferred work pending: record 1's stored calculation
/// Doubled as "99" and the work list that recomputes it (`[T, 03, 01,
/// fd]`); the list's path, and the list.
fn with_deferred_work(tree: &mut OwnedNode) -> (Vec<Vec<u8>>, OwnedNode) {
    let table = uint_segment(fixture::TABLE);
    let deferred = vec![table.clone(), vec![0x03], vec![0x01], vec![0xfd]];
    let doubled = [
        table.clone(),
        vec![0x05],
        serial::encode(1),
        uint_segment(fixture::DOUBLED),
    ];
    let path: Vec<&[u8]> = doubled.iter().map(Vec::as_slice).collect();
    tree.put(&path, text_node(&[], "99"));
    let work = OwnedNode::list(
        vec![0xfd],
        vec![OwnedNode::leaf(
            uint_segment(fixture::COUNT),
            0x40u32.to_be_bytes().to_vec(),
        )],
    );
    let deferred_path: Vec<&[u8]> = deferred.iter().map(Vec::as_slice).collect();
    tree.put(&deferred_path, work.clone());
    (deferred, work)
}

/// The review's P1 on recovery: deferred field work a file is opened with
/// is finished and saved as one operation, so when that save fails the
/// work is taken back. The file is served as its disk holds it, the work
/// list there and the stored calculation as stored, read-only, as when its
/// work cannot be done; the file on disk is unchanged.
#[test]
fn deferred_work_whose_save_fails_at_opening_is_taken_back_and_the_file_served_read_only() {
    let mut tree = fixture::tree(&[], true);
    fixture::add_tasks(&mut tree, 1..=3);
    let (deferred, work) = with_deferred_work(&mut tree);
    let doubled = vec![
        uint_segment(fixture::TABLE),
        vec![0x05],
        serial::encode(1),
        uint_segment(fixture::DOUBLED),
    ];
    let disk = fixture::TempFile::from_tree("engine-recovery-failing", &tree);
    let bytes = std::fs::read(&disk.0).unwrap();
    let file = local(&disk);
    file.fail_saves(Some(crate::store::SaveFault::BeforeRename));
    file.recover(&engine());
    file.fail_saves(None);
    assert_eq!(
        file.read_store().read_only_cause(),
        Some(crate::store::ReadOnlyCause::DeferredWork)
    );
    assert_eq!(
        node_at(&file, &deferred),
        Some(work.clone()),
        "the work list is back"
    );
    assert_eq!(
        node_at(&file, &doubled).and_then(|n| n.value),
        text_node(&[], "99").value,
        "as stored"
    );
    assert!(!file.read_store().has_unsaved_changes());
    assert_eq!(std::fs::read(&disk.0).unwrap(), bytes);
    // So on a server, whose file is hosted with something at its save's
    // copy name: the work's save fails there, and is taken back.
    drop(file);
    let copy = crate::store::save_copy_path(&disk.0);
    std::fs::write(&copy, b"half a save").unwrap();
    let file = load(&disk);
    assert_eq!(
        file.read_store().read_only_cause(),
        Some(crate::store::ReadOnlyCause::DeferredWork)
    );
    assert_eq!(node_at(&file, &deferred), Some(work.clone()));
    assert_eq!(std::fs::read(&disk.0).unwrap(), bytes);
    assert_eq!(std::fs::read(&copy).unwrap(), b"half a save", "left");
    std::fs::remove_file(&copy).unwrap();
    // Hosted again with saves working, it is recovered.
    drop(file);
    let file = load(&disk);
    assert!(engine().read_only(&file).is_none());
    assert!(node_at(&file, &deferred).is_none());
    assert_eq!(
        node_at(&file, &doubled).and_then(|n| n.value),
        text_node(&[], "2").value
    );
}

/// The review's P0 on recovery left by a panic: the engine takes back what
/// the panicking run did, and the work is marked begun, not ended, so the
/// next login does not get the file as if it were recovered: it is served
/// read-only, as its disk holds it with the work pending.
#[test]
fn deferred_work_a_panic_left_is_not_taken_as_done() {
    let mut tree = fixture::tree(&[], true);
    fixture::add_tasks(&mut tree, 1..=3);
    let (deferred, work) = with_deferred_work(&mut tree);
    let disk = fixture::TempFile::from_tree("engine-recovery-panic", &tree);
    let bytes = std::fs::read(&disk.0).unwrap();
    let file = local(&disk);
    let engine = engine();
    super::transaction::PANIC_RECOVERING.with(|panics| panics.set(true));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| file.recover(&engine)));
    super::transaction::PANIC_RECOVERING.with(|panics| panics.set(false));
    assert!(ran.is_err());
    assert_eq!(node_at(&file, &deferred), Some(work.clone()), "taken back");
    assert!(engine.read_only(&file).is_none());
    // The next login.
    file.recover(&engine);
    assert_eq!(
        file.read_store().read_only_cause(),
        Some(crate::store::ReadOnlyCause::DeferredWork)
    );
    assert_eq!(node_at(&file, &deferred), Some(work), "still pending");
    assert_eq!(std::fs::read(&disk.0).unwrap(), bytes);
}

/// The review's P0 on the decoded schema: taking a change back puts back
/// the schema the store read before it, as it was. An upload that leaves
/// the base tables catalog undecodable is stored, and the file is served
/// with the schema read before it; a change that repairs the catalog and
/// adds a table, whose save fails, leaves that schema, not the one it
/// decoded, so an op71 for the table it added is refused and writes
/// nothing.
#[test]
fn a_change_taken_back_leaves_the_schema_the_store_read_before_it() {
    use om_query::Write;
    let mut tree = fixture::tree(&[], true);
    fixture::add_tasks(&mut tree, 1..=3);
    let (_disk, file) = host("engine-schema-back", &tree);
    let engine = engine();
    let table = uint_segment(fixture::TABLE);
    let name = vec![vec![3], vec![16], vec![5], table.clone(), vec![16]];
    let undecodable = vec![0x0e ^ om_scsu::MASK];
    assert!(om_scsu::decode_scsu(&undecodable).is_err());
    let before = file.read_store().catalog().clone();
    let upload = Operation::UploadToCatalog(UploadToCatalog {
        path: name.clone(),
        node: OwnedNode::leaf(vec![16], undecodable),
    });
    assert_eq!(
        engine.submit(&file, upload).reply,
        Reply::UploadToCatalog(Ok(true))
    );
    assert_eq!(
        file.read_store().catalog(),
        &before,
        "served as read before"
    );

    let added = uint_segment(0x8003);
    let repair = Draft::ChangeSchema {
        writes: vec![
            Write::Put {
                path: name,
                node: text_node(&[16], "Tasks"),
            },
            Write::Put {
                path: vec![vec![3], vec![16], vec![5], added.clone()],
                node: OwnedNode::list(added.clone(), vec![text_node(&[16], "Added")]),
            },
        ],
    };
    file.fail_saves(Some(crate::store::SaveFault::BeforeRename));
    let repaired = engine.submit_draft(&file, repair);
    file.fail_saves(None);
    assert!(is_storage_error(&repaired.reply), "{repaired:?}");
    assert_eq!(file.read_store().catalog(), &before);

    let user = LockUser {
        connection: 1,
        txn: 7,
    };
    let reserve = Operation::ReserveRecordIds(ReserveRecordIds {
        user,
        holder: "A (Admin)".into(),
        request: CreateRecord {
            table: vec![added.clone()],
            count: 1,
            txn: user.txn,
        },
        room: writes::MAX_LOCKS,
        durable: true,
    });
    let Reply::ReserveRecordIds(reserved) = engine.submit(&file, reserve).reply else {
        panic!("not op71's reply");
    };
    assert_ne!(reserved.status, 0, "no such table");
    assert_eq!(node_at(&file, &[added]), None);
}

/// The review's P2 on account edits: an edit whose save's outcome is
/// unknown says so, and not that nothing was written.
#[test]
fn an_account_edit_whose_outcome_is_unknown_says_so() {
    let mut tree = fixture::tree(&[], true);
    fixture::add_local_accounts(&mut tree);
    index_accounts(&mut tree);
    let (_disk, file) = host("engine-account-unknown", &tree);
    file.fail_saves(Some(crate::store::SaveFault::FolderFlush));
    let outcome = engine().submit(
        &file,
        Operation::DeleteAccount(DeleteAccount {
            account: fixture::CLERK.into(),
            stamp: None,
        }),
    );
    file.fail_saves(None);
    assert!(
        matches!(
            &outcome.reply,
            Reply::DeleteAccount(Err(AccountEditError::OutcomeUnknown(_)))
        ),
        "{:?}",
        outcome.reply
    );
    // The file keeps the edit, as the renamed file holds it.
    assert!(file.account(fixture::CLERK).is_none());
    assert!(file.writes_refused().is_some());
}

/// An account edit whose save is not written puts back the accounts from
/// before it with the rest of the derived state (`Store::follow_accounts`,
/// `Store::roll_back`): the accounts a login reads through a view
/// (`engine::Read::derived`) and those published for readers that do not
/// take the store (`Hosted::account`) take the old password and not the
/// new one. One whose outcome is unknown keeps the edit, as the renamed
/// file holds it, and the accounts follow it.
#[test]
fn a_failed_save_of_an_account_edit_puts_back_the_accounts() {
    const CLERK_NEW: &str = "clerk's new secret";
    let mut tree = fixture::tree(&[], true);
    fixture::add_local_accounts(&mut tree);
    index_accounts(&mut tree);
    let (_disk, file) = host("engine-account-taken-back", &tree);
    let engine = engine();
    let reset = |fault| {
        file.fail_saves(Some(fault));
        let reset = ResetPassword {
            account: fixture::CLERK.into(),
            expire: false,
            stamp: None,
        };
        let given = Given::Reset(CLERK_NEW).credentials();
        let reply = engine
            .submit_with(&file, Operation::ResetPassword(reset), given)
            .reply;
        file.fail_saves(None);
        reply
    };
    // Whether `password` opens Clerk, by the accounts a login reads and by
    // the published ones, which agree.
    let opens = |password: &str| {
        let found = engine.read(&file).read().derived.account(fixture::CLERK);
        let read =
            crate::hosting::check_password(&file.name, fixture::CLERK, found.unwrap(), password);
        let published = file.verify_password(fixture::CLERK, password);
        assert_eq!(read, published);
        read.is_some()
    };
    let reply = reset(crate::store::SaveFault::BeforeRename);
    assert!(
        matches!(
            reply,
            Reply::ResetPassword(Err(AccountEditError::Unwritten(_)))
        ),
        "{reply:?}"
    );
    assert!(opens(fixture::CLERK_PASSWORD));
    assert!(!opens(CLERK_NEW));
    let reply = reset(crate::store::SaveFault::FolderFlush);
    assert!(
        matches!(
            reply,
            Reply::ResetPassword(Err(AccountEditError::OutcomeUnknown(_)))
        ),
        "{reply:?}"
    );
    assert!(opens(CLERK_NEW));
    assert!(!opens(fixture::CLERK_PASSWORD));
}

/// The review's P2 on panics: a record commit that panics part way, inside
/// its own guard, on a file without unsaved changes is put back by that
/// guard and then by the engine's, and the file is left as it was. It
/// counts as changed since its last save, though (the review of #751): an
/// unwind can leave what no journal holds, so memory is no longer known to
/// equal the file on disk, and the next save writes it.
#[test]
fn a_commit_that_panics_is_put_back_and_counts_as_changed() {
    let mut tree = fixture::tree(&[], true);
    fixture::add_tasks(&mut tree, 1..=3);
    let (_disk, file) = host("engine-panic-clean", &tree);
    let before = untouched(&file, &_disk);
    assert!(!before.unsaved);
    let table = uint_segment(fixture::TABLE);
    let user = LockUser {
        connection: 1,
        txn: 7,
    };
    let record = vec![table.clone(), vec![0x05], serial::encode(1)];
    let edited = OwnedNode::list(
        serial::encode(1),
        vec![
            text_node(&uint_segment(fixture::NAME), "Never kept"),
            serial_leaf(MOD_COUNT, 2),
        ],
    );
    let list = OwnedNode::list(
        Vec::new(),
        vec![OwnedNode::list(
            table.clone(),
            vec![OwnedNode::list(
                vec![0x05],
                vec![OwnedNode::list(
                    serial::encode(1),
                    vec![OwnedNode::leaf(uint_segment(fixture::NAME), Vec::new())],
                )],
            )],
        )],
    );
    let commit = Operation::CommitRecords(CommitRecords {
        user,
        account: fixture::ACCOUNT.into(),
        access: file
            .account(fixture::ACCOUNT)
            .unwrap()
            .access
            .clone()
            .unwrap(),
        list,
        staged: Staged::from([(record, edited)]),
        withheld: Withheld::default(),
    });
    super::transaction::PANIC_WRITING_RECORDS.with(|panics| panics.set(true));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        engine().submit(&file, commit)
    }));
    super::transaction::PANIC_WRITING_RECORDS.with(|panics| panics.set(false));
    assert!(ran.is_err());
    assert_eq!(
        untouched(&file, &_disk),
        Untouched {
            unsaved: true,
            ..before
        }
    );
}

/// An export copies the file on disk while the store counts nothing as
/// changed since its last save (`engine::export`), so that count must be
/// exact after a save that is not written: the journal keeps the parents a
/// change creates as absent, and the roll-back takes them back with it
/// (docs/server-storage.md 5, "Taking a change back"). The first record of
/// a table without one creates its record list `[T, 05]` on the way; its
/// commit, whose save fails before its rename, is taken back whole, the
/// list too, and an export of the version held then, a copy of the file on
/// disk, holds exactly the tree in memory.
#[test]
fn an_export_after_a_failed_save_holds_what_memory_holds() {
    // Notes without its records: its record list, record ids and indexes.
    let table = uint_segment(fixture::NOTES);
    let mut tree = fixture::tree(&[], true);
    for key in [0x05u8, 0x0d, 0x01] {
        tree.remove(&[&table, &[key]]);
    }
    let (disk, file) = host("engine-export-after-failure", &tree);
    let records = vec![table.clone(), vec![0x05]];
    assert_eq!(node_at(&file, &records), None, "no record list yet");
    let engine = engine();
    let user = LockUser {
        connection: 1,
        txn: 7,
    };
    let reserve = Operation::ReserveRecordIds(ReserveRecordIds {
        user,
        holder: "A (Admin)".into(),
        request: CreateRecord {
            table: vec![table.clone()],
            count: 1,
            txn: user.txn,
        },
        room: writes::MAX_LOCKS,
        durable: true,
    });
    let Reply::ReserveRecordIds(reserved) = engine.submit(&file, reserve).reply else {
        panic!("not op71's reply");
    };
    assert_eq!(reserved.status, 0);
    let id = reserved.record[0].clone();
    let new = OwnedNode::list(
        id.clone(),
        vec![
            text_node(&uint_segment(fixture::BODY), "First"),
            serial_leaf(MOD_COUNT, 1),
        ],
    );
    let list = OwnedNode::list(
        Vec::new(),
        vec![OwnedNode::list(
            table.clone(),
            vec![OwnedNode::list(
                vec![0x05],
                vec![OwnedNode::leaf(id.clone(), vec![COMMIT_NEW])],
            )],
        )],
    );
    let commit = Operation::CommitRecords(CommitRecords {
        user,
        account: fixture::ACCOUNT.into(),
        access: file
            .account(fixture::ACCOUNT)
            .unwrap()
            .access
            .clone()
            .unwrap(),
        list,
        staged: Staged::from([(vec![table.clone(), vec![0x05], id.clone()], new)]),
        withheld: Withheld::default(),
    });
    file.fail_saves(Some(crate::store::SaveFault::BeforeRename));
    let failed = engine.submit(&file, commit);
    file.fail_saves(None);
    assert!(is_storage_error(&failed.reply), "{:?}", failed.reply);
    assert_eq!(
        node_at(&file, &records),
        None,
        "the record list is taken back"
    );
    exported_from_disk_as_memory(&engine, &file, &disk);
}

/// An export of the version `file` holds now is a copy of its file on disk
/// (`disk`), byte for byte, and holds exactly the tree in memory.
fn exported_from_disk_as_memory(engine: &FileEngine, file: &Hosted, disk: &fixture::TempFile) {
    use super::export::{Kind, Progress, Source, Target};
    assert!(matches!(
        engine.read(file).export_source().unwrap(),
        Source::OnDisk(_)
    ));
    let path = disk.0.with_extension("export");
    let mut into = std::fs::File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    engine
        .export(
            file,
            Kind::Current,
            Target::File(&mut into),
            Progress::none(),
        )
        .unwrap();
    drop(into);
    let exported = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        exported,
        std::fs::read(&disk.0).unwrap(),
        "the file on disk"
    );
    let read = om_hbam::HbamFile::from_bytes(exported).unwrap();
    assert_eq!(
        Some(read.key_tree().unwrap().root.to_owned_node()),
        node_at(file, &[]),
        "what memory holds"
    );
}

/// The review of #751 (P0): a roll-back puts back what a change removed as
/// it was kept, byte for byte and kind for kind, not through the checks
/// that guard a client's write. A record held as a value, not a list,
/// whose deletion's save fails before its rename, is back as that value,
/// the store counts as unchanged since its last save, and an export, a
/// copy of the file on disk, holds exactly what memory holds.
#[test]
fn a_deletion_taken_back_puts_the_record_back_as_it_was() {
    let mut tree = fixture::tree(&[], true);
    fixture::add_tasks(&mut tree, 1..=3);
    let table = uint_segment(fixture::TABLE);
    let held = vec![table.clone(), vec![0x05], serial::encode(2)];
    let path: Vec<&[u8]> = held.iter().map(Vec::as_slice).collect();
    tree.put(
        &path,
        OwnedNode::leaf(Vec::new(), b"held as a value".to_vec()),
    );
    let (disk, file) = host("engine-deletion-back", &tree);
    let before = node_at(&file, &[]);
    assert!(node_at(&file, &held).is_some_and(|record| !record.push));
    let engine = engine();
    let list = OwnedNode::list(
        Vec::new(),
        vec![OwnedNode::list(
            table.clone(),
            vec![OwnedNode::list(
                vec![0x05],
                vec![OwnedNode::leaf(serial::encode(2), vec![COMMIT_DELETED])],
            )],
        )],
    );
    let commit = Operation::CommitRecords(CommitRecords {
        user: LockUser {
            connection: 1,
            txn: 7,
        },
        account: fixture::ACCOUNT.into(),
        access: file
            .account(fixture::ACCOUNT)
            .unwrap()
            .access
            .clone()
            .unwrap(),
        list,
        staged: Staged::default(),
        withheld: Withheld::default(),
    });
    file.fail_saves(Some(crate::store::SaveFault::BeforeRename));
    let failed = engine.submit(&file, commit);
    file.fail_saves(None);
    assert!(is_storage_error(&failed.reply), "{:?}", failed.reply);
    assert_eq!(node_at(&file, &[]), before, "the record is back as it was");
    assert!(!file.read_store().has_unsaved_changes());
    exported_from_disk_as_memory(&engine, &file, &disk);
}

/// A source whose draws panic from the third on: in deferred work that
/// stores container data, the operation draws its records' environment (a
/// UUID and a seed), then a new library's unique id, once the library and
/// its header are made (`library::store_as`). It counts its draws.
struct PanickingDraws(Arc<AtomicU64>);

impl Environment for PanickingDraws {
    fn now(&self) -> jiff::Zoned {
        "2026-11-24T12:00:00[UTC]".parse().unwrap()
    }

    fn fill_random(&self, bytes: &mut [u8]) {
        if self.0.fetch_add(1, Ordering::Relaxed) >= 2 {
            panic!("a test's panic in a draw");
        }
        bytes.fill(7);
    }

    fn stopwatch(&self) -> om_data::rules::Stopwatch {
        virtual_stopwatch(Duration::from_millis(1), Arc::default())
    }
}

/// The review of #751 (P0): a panic part way through deferred work that
/// stores container data, in the draw of a new library member's unique id,
/// leaves nothing of the work: the record's own journal is taken back with
/// the operation's. And whatever an unwind leaves, the store counts as
/// changed since its last save, so an export walks the tree rather than
/// copy the file on disk.
#[test]
fn a_panic_in_deferred_container_work_leaves_nothing_and_counts_as_changed() {
    use super::export::Source;
    const GENERATED: u64 = 48;
    let mut tree = fixture::tree(&[], true);
    let table = uint_segment(fixture::TABLE);
    fixture::add_field(
        &mut tree,
        fixture::TABLE,
        GENERATED,
        "Generated",
        2,
        6,
        0,
        0x20,
        0,
    );
    fixture::add_formula(
        &mut tree,
        &fixture::names(),
        (fixture::TABLE, fixture::OCCURRENCE),
        GENERATED,
        5,
        "Base64Decode ( \"aGVsbG8=\" ; \"a.txt\" )",
    );
    tree.put(
        &[&table, &[0x03], &[0x01], &[0xfd], &uint_segment(GENERATED)],
        OwnedNode::leaf(Vec::new(), 0xc0u32.to_be_bytes().to_vec()),
    );
    assert!(tree.at(&[&table, &[0x1f]]).is_none(), "no library yet");
    let disk = fixture::TempFile::from_tree("engine-container-panic", &tree);
    let file = local(&disk);
    let before = node_at(&file, &[]);
    assert!(!file.read_store().has_unsaved_changes());
    let draws = Arc::new(AtomicU64::new(0));
    let engine = FileEngine::new(Box::new(PanickingDraws(Arc::clone(&draws))));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| file.recover(&engine)));
    assert!(ran.is_err());
    assert_eq!(
        draws.load(Ordering::Relaxed),
        3,
        "the panic is the library's draw"
    );
    assert_eq!(node_at(&file, &[]), before, "nothing of the work is left");
    assert!(
        file.read_store().has_unsaved_changes(),
        "counted as changed"
    );
    assert!(matches!(
        engine.read(&file).export_source().unwrap(),
        Source::Written { .. }
    ));
}

/// The review of #751 (P2): member keys a write already passed change
/// nothing, so a draft that writes only them leaves the file as it was:
/// not counted as changed, not saved, its change number where it was.
#[test]
fn a_member_key_draft_that_writes_nothing_leaves_the_file_as_it_was() {
    let mut tree = fixture::tree(&[], true);
    fixture::index_layouts(&mut tree);
    let (disk, file) = host("engine-keys-passed", &tree);
    let layouts = vec![vec![4]];
    assert!(file.read_store().issue_member_key(&layouts).is_some());
    // A write passes the key: the catalog's counters set past it, saved.
    {
        let mut store = file.write_store(&Writer::for_tests());
        for key in [crate::derived::LAST_MEMBER_ID, MOD_COUNT] {
            let at = vec![vec![4], vec![crate::derived::CATALOG_INDEX], vec![key]];
            assert!(store.put_subtree(&at, serial_leaf(key, 1_000_000)));
        }
        assert!(store.save(&file.at).unwrap());
    }
    let saves = file.read_store().saves();
    let bytes = std::fs::read(&disk.0).unwrap();
    let outcome = engine().submit_draft(&file, Draft::ChangeSchema { writes: Vec::new() });
    assert_eq!(outcome.reply, Reply::ChangeSchema(Ok(())));
    assert!(!file.read_store().has_unsaved_changes());
    assert_eq!(file.read_store().saves(), saves, "not saved");
    assert_eq!(std::fs::read(&disk.0).unwrap(), bytes, "the file as it was");
}

/// The review of #751 (P2): the test view reads the accounts, password
/// verifiers and all, only for a reader of them
/// (`Reading::serve_with_accounts`). Another reader that read them would
/// get none through it, so its two answers differ and the check fails; a
/// reader of them gets them through both views, the same.
#[test]
fn the_test_view_reads_the_accounts_only_for_a_reader_of_them() {
    let (_disk, file) = host("engine-accounts-asked", &fixture::tree(&[], true));
    let engine = engine();
    engine.check_reads();
    let has_accounts = |read: Read<'_>| read.derived.accounts().is_ok();
    assert!(engine.read(&file).serve_with_accounts(has_accounts));
    let unasked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        engine.read(&file).serve(has_accounts)
    }));
    assert!(unasked.is_err(), "the test view read the accounts unasked");
}
