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
/// released. Every step but the Save must change the tree, and the Save
/// must save the file, so an operation that did nothing fails here; every
/// edit's locks are released after it.
#[test]
fn a_recorded_sequence_of_design_and_container_operations_replays_to_the_same_replies_and_tree() {
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

    // The script's steps.
    let definition = vec![vec![17], vec![5], serial::encode(SCRIPT)];
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
    // Stamped at the environment's time, not the host's.
    let time = crate::connection::host_time_key_at(&Seeded(AtomicU64::new(0)).now());
    let stamp = [definition.clone(), vec![design::MOD_TIME.to_vec()]].concat();
    assert_eq!(
        node(&stamp).and_then(|n| n.value),
        Some([&[time.len() as u8][..], &time].concat())
    );

    // A new field.
    let fields = vec![uint_segment(fixture::TABLE), vec![3]];
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

    // A new layout, and edits of it.
    let layouts_index = vec![vec![4], vec![1]];
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
    let portal = design::PlacePortal {
        layout,
        expected: revision(&member),
        table: fixture::NOTES,
        occurrence: fixture::NOTES_OCCURRENCE,
        frame: frame(120.0, 20.0, 90.0, 300.0),
        rows: 3,
        fields: vec![fixture::BODY],
    };
    assert!(changed(&answer(submit(edit(DesignEdit::PlacePortal(
        portal
    ))))));
    let button = design::PlaceButton {
        layout,
        expected: revision(&member),
        script: SCRIPT,
        frame: frame(240.0, 20.0, 20.0, 80.0),
        label: "Greet".into(),
        parameter: None,
    };
    assert!(changed(&answer(submit(edit(DesignEdit::PlaceButton(
        button
    ))))));
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
    assert!(changed(&reply), "{reply:?}");
    let moved = design::SetObjectFrames {
        layout,
        expected: revision(&member),
        frames: vec![(text.field_object, frame(10.0, 40.0, 20.0, 120.0))],
    };
    let reply = answer(submit(edit(DesignEdit::SetObjectFrames(moved))));
    assert!(changed(&reply), "{reply:?}");
    let words = design::SetText {
        expected: revision(&member),
        layout,
        object: field.label_object,
        text: "Task".into(),
    };
    let reply = answer(submit(edit(DesignEdit::SetText(words))));
    assert!(changed(&reply), "{reply:?}");
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
    let deleted = design::DeleteObjects {
        layout,
        expected: revision(&member),
        objects: vec![text.field_object],
    };
    let reply = answer(submit(edit(DesignEdit::DeleteObjects(deleted))));
    assert!(changed(&reply), "{reply:?}");
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
    let width = design::LayoutWidth {
        expected: revision(&member),
        layout,
        width: 640.0,
    };
    let reply = answer(submit(edit(DesignEdit::LayoutWidth(width))));
    assert!(changed(&reply), "{reply:?}");
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
    let reply = answer(submit(edit(DesignEdit::LayoutSetup(setup))));
    assert!(changed(&reply), "{reply:?}");
    let copy = design::DuplicateLayout {
        expected: revision(&layouts_index),
        layout,
        uuid: [10; 16],
    };
    let reply = answer(submit(edit(DesignEdit::DuplicateLayout(copy))));
    let design::Reply::LayoutCreated(copy) = reply else {
        panic!("no copy: {reply:?}");
    };
    assert!(node(&layout_path(copy.layout)).is_some());
    let delete = design::DeleteLayout {
        expected: revision(&layouts_index),
        layout: copy.layout,
    };
    let reply = answer(submit(edit(DesignEdit::DeleteLayout(delete))));
    assert!(changed(&reply), "{reply:?}");
    assert!(node(&layout_path(copy.layout)).is_none());

    // A value list's values.
    let list = vec![vec![0x21], vec![5], vec![1]];
    let values = design::ValueListValues {
        expected: revision(&list),
        list: 1,
        values: vec!["Open".into(), "Done".into(), "Later".into()],
    };
    let reply = answer(submit(edit(DesignEdit::ValueListValues(values))));
    assert!(changed(&reply), "{reply:?}");

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
    let store = StoreContainerData {
        table: vec![tasks.clone()],
        data: Ok(file_data(b"a stored file")),
        bytes: 64,
    };
    let outcome = submit(Operation::StoreContainerData(store));
    let Reply::StoreContainerData(stored) = outcome.reply else {
        panic!("not op88's reply: {:?}", outcome.reply);
    };
    assert_eq!(stored.status, 0);
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
