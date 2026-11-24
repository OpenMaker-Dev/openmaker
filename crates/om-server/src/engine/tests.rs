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

    fn stopwatch(&self) -> om_data::rules::Stopwatch {
        std::sync::Arc::new(|| std::time::Duration::ZERO)
    }
}

fn engine() -> FileEngine {
    FileEngine::new(Box::new(Seeded(AtomicU64::new(0))))
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
    let host = |tag: &str| {
        let file = fixture::TempFile::from_tree(tag, &tree);
        let hosted = HostedFile {
            name: fixture::FILE.into(),
            path: file.0.clone(),
        };
        let hosting = Hosting::load(&[hosted], false, &EventLog::default()).unwrap();
        (file, hosting.by_name(fixture::FILE).unwrap())
    };
    let (_first, file) = host("engine-recorded");
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
    let (_second, again) = host("engine-replayed");
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

/// A fixed clock, a random source that gives `byte` for every byte, and a
/// stopwatch that reads `jump` from its second reading on.
struct Fixed {
    now: &'static str,
    byte: u8,
    jump: std::time::Duration,
}

impl Environment for Fixed {
    fn now(&self) -> jiff::Zoned {
        self.now.parse().unwrap()
    }

    fn fill_random(&self, bytes: &mut [u8]) {
        bytes.fill(self.byte);
    }

    fn stopwatch(&self) -> om_data::rules::Stopwatch {
        let (read, jump) = (std::sync::atomic::AtomicBool::new(false), self.jump);
        std::sync::Arc::new(move || {
            if read.swap(true, Ordering::Relaxed) {
                jump
            } else {
                std::time::Duration::ZERO
            }
        })
    }
}

/// #692: the formulas an operation evaluates take the date, `Random` and
/// their time budget from the engine's environment. An op70 whose record
/// limit reads the date (as `Get` answers it, and as a date without a year
/// takes it) and `Random`, in a year other than the system clock's,
/// replays to the same outcome under the same environment, and is refused
/// under another date, under a seed whose first `Random` is past the
/// limit's, and under a stopwatch that meets the budget while the
/// formula's loop runs.
#[test]
fn a_record_limit_takes_the_date_random_and_its_budget_from_the_environment() {
    use om_data::host::privilege_set::{
        AccessLevel, Actions, Allow, CustomRules, MenuCommands, TableRules,
    };
    use std::time::Duration;

    let mut tree = fixture::tree(&[], true);
    fixture::add_tasks(&mut tree, 1..=1);
    let names = fixture::names();
    let mut context = om_formula::Context::new(&names);
    context.occurrence = Some(fixture::OCCURRENCE);
    let limit = om_formula::compile(
        "While ( [ i = 0 ] ; i < 100 ; [ i = i + 1 ] ; i ) = 100 \
         and Get ( CurrentDate ) = Date ( 11 ; 24 ; 2031 ) \
         and GetAsDate ( \"11/24\" ) = Get ( CurrentDate ) and Random < .5",
        &context,
    )
    .unwrap();
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
    let operation = Operation::CommitRecords(CommitRecords {
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
    });
    // Each run on a fresh load of the file.
    let commit = |environment: Fixed| {
        let file = fixture::TempFile::from_tree("engine-limit", &tree);
        let hosted = HostedFile {
            name: fixture::FILE.into(),
            path: file.0.clone(),
        };
        let hosting = Hosting::load(&[hosted], false, &EventLog::default()).unwrap();
        let hosted = hosting.by_name(fixture::FILE).unwrap();
        let outcome = FileEngine::new(Box::new(environment)).submit(&hosted, operation.clone());
        let stored = hosted.read_store().node(&[]).cloned();
        (outcome, stored)
    };
    // The engine seeds the operation's generator from eight bytes of the
    // environment's random source.
    let first_random = |byte: u8| om_calc::eval::Lcg(u64::from_le_bytes([byte; 8])).next_f64();
    let (below, past) = (7, 1);
    assert!(first_random(below) < 0.5 && first_random(past) >= 0.5);
    let today = "2031-11-24T12:00:00[UTC]";
    let at = |now, byte, jump| Fixed { now, byte, jump };
    let (recorded, tree_after) = commit(at(today, below, Duration::ZERO));
    assert_eq!(
        recorded.reply,
        Reply::CommitRecords(CommitReply {
            result: Ok(()),
            unused: Staged::new(),
        })
    );
    assert_eq!(
        commit(at(today, below, Duration::ZERO)),
        (recorded, tree_after)
    );
    for environment in [
        at("2031-11-25T12:00:00[UTC]", below, Duration::ZERO),
        at(today, past, Duration::ZERO),
        at(today, below, Duration::from_secs(1)),
    ] {
        let (now, byte, jump) = (environment.now, environment.byte, environment.jump);
        let (outcome, _) = commit(environment);
        assert!(
            matches!(
                &outcome.reply,
                Reply::CommitRecords(CommitReply {
                    result: Err(CommitError::Refused(_)),
                    ..
                })
            ),
            "{now} {byte} {jump:?}: {:?}",
            outcome.reply
        );
    }
}
