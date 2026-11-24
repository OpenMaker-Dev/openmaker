use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

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
/// times `step`; `readings` counts them.
fn virtual_stopwatch(step: Duration, readings: Arc<AtomicU64>) -> om_data::rules::Stopwatch {
    Arc::new(move || {
        let n = readings.fetch_add(1, Ordering::Relaxed);
        step * u32::try_from(n).unwrap_or(u32::MAX)
    })
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
/// virtual stopwatch advancing by `step`, whose readings it counts.
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
    let hosting = Hosting::load(&[hosted], false, &EventLog::default()).unwrap();
    let hosted = hosting.by_name(fixture::FILE).unwrap();
    let outcome = FileEngine::new(Box::new(environment)).submit(&hosted, operation.clone());
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
