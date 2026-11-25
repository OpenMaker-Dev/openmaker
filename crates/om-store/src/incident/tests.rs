//! The incident journal against crashes and damage, on the simulated disk.

use super::*;
use crate::log::{FileFate, Position};
use crate::pagefile::MAX_RESTORE_POINTS;
use crate::sim::{Fates, Fault, Sim};

fn position(index: u64) -> Position {
    Position {
        index,
        term: 1,
        hash: [index as u8; 32],
    }
}

fn checkpoint(number: u64) -> CheckpointId {
    CheckpointId {
        number,
        meta: (number + 10, Hash([number as u8; 32])),
    }
}

/// One incident of every kind.
fn every_kind() -> Vec<What> {
    vec![
        What::checkpoints(
            &GiveUp {
                checkpoints: vec![
                    GivenUp {
                        checkpoint: checkpoint(7),
                        lost: Lost::Points(vec![("point é".into(), 7), ("other".into(), 6)]),
                    },
                    GivenUp {
                        checkpoint: checkpoint(6),
                        lost: Lost::Unknown,
                    },
                ],
                slots: vec![1, 3],
                onto: checkpoint(5),
            },
            true,
            false,
            "a checkpoint does not check",
        ),
        What::LogDamage(Box::new(DamagePlan {
            start: position(4),
            kept: position(9),
            marker: 12,
            cut_count_before: 1,
            cut_count: Some(2),
            listed_before: Segments { first: 1, count: 4 },
            listed: Segments { first: 2, count: 2 },
            opens_at: position(9),
            files: vec![
                FileFate {
                    number: 1,
                    first: Some(1),
                    last: Some(3),
                    fate: Fate::SetAside {
                        reason: SET_ASIDE_REASONS[1],
                    },
                },
                FileFate {
                    number: 2,
                    first: Some(4),
                    last: None,
                    fate: Fate::Lengthened { size: 4096 },
                },
                FileFate {
                    number: 3,
                    first: None,
                    last: None,
                    fate: Fate::Kept,
                },
                FileFate {
                    number: 4,
                    first: None,
                    last: Some(20),
                    fate: Fate::Missing,
                },
                FileFate {
                    number: 5,
                    first: Some(21),
                    last: Some(30),
                    fate: Fate::MovedEarlier {
                        to: "log/given-up-1.0000000000000005".into(),
                    },
                },
                // A run of one fate, one row: its entries at its ends.
                FileFate {
                    number: 6,
                    first: Some(31),
                    last: None,
                    fate: Fate::Missing,
                },
                FileFate {
                    number: 7,
                    first: None,
                    last: None,
                    fate: Fate::Missing,
                },
                FileFate {
                    number: 8,
                    first: None,
                    last: Some(40),
                    fate: Fate::Missing,
                },
            ],
            tail_copied: Some(3),
            before_start: Some(ChainFault {
                segment: 2,
                offset: 300,
                index: 3,
                unreadable: true,
            }),
            prefix: Some("given-up-2.".into()),
        })),
        What::UnreadableEnd {
            last: position(30),
            end: UnreadableEnd {
                segment: 4,
                offset: 900,
                index: 31,
            },
        },
        What::LastBatch {
            marker: Marker {
                index: 40,
                hash: [4; 32],
                committed: 39,
            },
            cut: Some(Cut {
                index: 41,
                segment: 5,
                offset: 1000,
                dropped: 1,
                why: CutWhy::NotWhole,
            }),
            unreadable_end: None,
        },
        What::JournalLost,
    ]
}

/// The journal, for a call made with the store closed (start count 0, so
/// numbers from 1).
fn opened(sim: &Sim) -> Journal {
    Journal::open(&sim.fs(), 0).unwrap()
}

/// Every kind reads back as it was written, in order, numbered from 1, and
/// numbers go on after an acknowledgement: the journal then holds only what
/// was not acknowledged.
#[test]
fn every_kind_reads_back_and_numbers_go_on_after_an_acknowledgement() {
    let sim = Sim::new(1);
    let fs = sim.fs();
    fs.create_root().unwrap();
    let _lock = fs.lock().unwrap();
    let mut journal = opened(&sim);
    assert!(journal.found().missing);
    let mut written = Vec::new();
    for what in every_kind() {
        written.push(journal.record(&fs, what).unwrap());
    }
    assert_eq!(
        written.iter().map(|i| i.number).collect::<Vec<_>>(),
        [1, 2, 3, 4, 5]
    );
    sim.restart();
    let journal = opened(&sim);
    assert!(journal.found().is_clean(), "{:?}", journal.found());
    assert_eq!(journal.incidents(), &written[..]);
    let mut journal = journal;
    let clean = Found::default();
    assert_eq!(journal.acknowledge(&fs, 3, &clean).unwrap(), 3);
    assert_eq!(journal.incidents(), &written[3..]);
    let next = journal.record(&fs, What::JournalLost).unwrap();
    assert_eq!(next.number, 6);
    sim.restart();
    let mut journal = opened(&sim);
    assert_eq!(journal.incidents().len(), 3);
    assert_eq!(journal.incidents()[2], next);
    // All of them: none left, and numbers still go on.
    assert_eq!(journal.acknowledge(&fs, 6, &clean).unwrap(), 3);
    sim.restart();
    let mut journal = opened(&sim);
    assert!(journal.incidents().is_empty());
    assert!(!journal.found().missing, "the base record holds the name");
    assert_eq!(journal.record(&fs, What::JournalLost).unwrap().number, 7);
    // Only a number the journal holds, and only as shown.
    for (through, shown) in [
        (8, clean.clone()),
        (6, clean.clone()),
        (
            7,
            Found {
                damaged: vec![512],
                damaged_count: 1,
                ..Found::default()
            },
        ),
    ] {
        assert!(matches!(
            journal.acknowledge(&fs, through, &shown),
            Err(JournalError::Refused(_))
        ));
    }
    assert_eq!(journal.incidents().len(), 1);
}

/// An incident of several sectors, so that a crash can tear it.
fn large() -> What {
    What::checkpoints(
        &GiveUp {
            checkpoints: Vec::new(),
            slots: vec![2],
            onto: checkpoint(3),
        },
        false,
        true,
        &"x".repeat(MAX_TEXT),
    )
}

/// A crash at every step of an append, each settled three ways: the
/// records before it are read whole; the one cut short is read whole or
/// said to be a place that does not check, after the last record; the
/// next record goes after it, and nothing before is lost.
#[test]
fn a_journal_with_a_torn_last_record_reads_every_record_before_it() {
    let build = |seed: u64| {
        let sim = Sim::new(seed);
        let fs = sim.fs();
        fs.create_root().unwrap();
        let lock = fs.lock().unwrap();
        let mut journal = opened(&sim);
        let mut before = Vec::new();
        for what in every_kind().into_iter().take(2) {
            before.push(journal.record(&fs, what).unwrap());
        }
        drop(lock);
        sim.restart();
        (sim, before)
    };
    let steps = {
        let (sim, _) = build(0);
        let fs = sim.fs();
        let _lock = fs.lock().unwrap();
        let mut journal = opened(&sim);
        let begin = sim.steps();
        journal.record(&fs, large()).unwrap();
        sim.steps() - begin
    };
    let mut torn = 0;
    for step in 1..=steps {
        for fates in [Fates::KeepAll, Fates::LoseAll, Fates::Drawn] {
            for seed in 0..24 {
                let (sim, before) = build(seed);
                let fs = sim.fs();
                let lock = fs.lock().unwrap();
                let mut journal = opened(&sim);
                sim.set_crash_fates(fates);
                sim.place(sim.steps() + step, Fault::Crash);
                let cut_short = journal.record(&fs, large()).is_err();
                assert!(cut_short, "step {step}: the crash fired");
                drop(lock);
                sim.restart();
                let _lock = fs.lock().unwrap();
                let mut journal = opened(&sim);
                let read = journal.incidents().to_vec();
                assert_eq!(
                    &read[..2],
                    &before[..],
                    "step {step}, {fates:?}, seed {seed}"
                );
                assert!(read.len() <= 3, "step {step}");
                // Read whole, it is what was written, and nothing else.
                if let Some(third) = read.get(2) {
                    assert_eq!(
                        third,
                        &Incident {
                            number: 3,
                            start_count: 0,
                            what: large(),
                        },
                        "step {step}, {fates:?}, seed {seed}"
                    );
                }
                let found = journal.found().clone();
                if read.len() == 2 && !found.is_clean() {
                    torn += 1;
                    assert!(found.ends_damaged, "step {step}: {found:?}");
                }
                // The next record goes after whatever is there.
                let next = journal.record(&fs, What::JournalLost).unwrap();
                sim.restart();
                let journal = opened(&sim);
                assert_eq!(journal.incidents().last(), Some(&next));
                assert_eq!(&journal.incidents()[..2], &before[..]);
            }
        }
    }
    assert!(torn > 0, "no append was torn");
}

/// A record damaged after it was written is passed over and said; the
/// records after it are read; a header that claims more than a record
/// holds reads nothing past the file.
#[test]
fn a_damaged_record_is_passed_over_and_said() {
    let sim = Sim::new(3);
    let fs = sim.fs();
    fs.create_root().unwrap();
    let _lock = fs.lock().unwrap();
    let mut journal = opened(&sim);
    let mut written = Vec::new();
    for what in every_kind() {
        written.push(journal.record(&fs, what).unwrap());
    }
    // The second record's payload.
    let second = encode_record(&written[0]).unwrap().len() as u64;
    sim.damage(FILE, second + 40, 4).unwrap();
    // The last record's length claims more than the file holds.
    let size = sim
        .fs()
        .open(FILE, FileOptions::PLAIN)
        .unwrap()
        .size()
        .unwrap();
    let last = size - encode_record(&written[4]).unwrap().len() as u64;
    let file = fs.open(FILE, FileOptions::PLAIN).unwrap();
    file.write_at(last + 12, &u32::MAX.to_le_bytes()).unwrap();
    file.flush().unwrap();
    sim.restart();
    let journal = opened(&sim);
    let numbers: Vec<u64> = journal.incidents().iter().map(|i| i.number).collect();
    assert_eq!(numbers, [1, 3, 4]);
    // Each sector of the damaged second record, and the last.
    let third = second + encode_record(&written[1]).unwrap().len() as u64;
    let damaged = &journal.found().damaged;
    assert_eq!(damaged.first(), Some(&second));
    assert_eq!(damaged.last(), Some(&last));
    assert!(
        damaged.iter().all(|&at| at < third || at == last),
        "{damaged:?}"
    );
    assert!(journal.found().ends_damaged);
}

/// Records the device fails to read are taken as zeros, and said so; the
/// next record's number skips past every number they could hold, so none is
/// used twice once they read again.
#[test]
fn a_record_the_device_fails_to_read_keeps_its_number() {
    let sim = Sim::new(4);
    let fs = sim.fs();
    fs.create_root().unwrap();
    let _lock = fs.lock().unwrap();
    let mut journal = opened(&sim);
    let mut written = Vec::new();
    for what in every_kind().into_iter().take(2) {
        written.push(journal.record(&fs, what).unwrap());
    }
    sim.restart();
    // The block read, then each of its two sectors, fails.
    for _ in 0..1 + 2 {
        sim.place(sim.steps() + 1, Fault::FailRead);
    }
    let mut journal = opened(&sim);
    assert!(journal.incidents().is_empty());
    assert!(
        !journal.found().unreadable.is_empty(),
        "{:?}",
        journal.found()
    );
    let next = journal.record(&fs, What::JournalLost).unwrap();
    assert!(next.number > written[1].number, "{next:?}");
    sim.restart();
    let journal = opened(&sim);
    let numbers: Vec<u64> = journal.incidents().iter().map(|i| i.number).collect();
    assert_eq!(numbers, [1, 2, next.number]);
}

/// After an acknowledgement the journal's only record of the numbers used
/// is its base record; that damaged, the numbers an open gives still come
/// after every earlier one, from its start count.
#[test]
fn a_number_never_comes_again_when_the_base_record_is_damaged() {
    let sim = Sim::new(5);
    let fs = sim.fs();
    fs.create_root().unwrap();
    let _lock = fs.lock().unwrap();
    let mut journal = Journal::open(&fs, 1).unwrap();
    let first = journal.record(&fs, What::JournalLost).unwrap();
    assert_eq!(first.number, PER_START);
    journal
        .acknowledge(&fs, first.number, &Found::default())
        .unwrap();
    sim.damage(FILE, 40, 4).unwrap();
    sim.restart();
    let mut journal = Journal::open(&fs, 2).unwrap();
    assert!(!journal.found().damaged.is_empty());
    let next = journal.record(&fs, What::JournalLost).unwrap();
    assert!(next.number > first.number, "{next:?} after {first:?}");
}

/// An acknowledgement drops only what the read it was shown named: for each
/// read the device fails during the call, once or with one of the sector
/// reads after it, the call is accepted or refused, and after it every
/// record after the acknowledged number that a clean read showed is still
/// there.
#[test]
fn an_acknowledgement_keeps_every_record_after_it_whatever_read_fails() {
    // Five records, the second damaged.
    let build = || {
        let sim = Sim::new(6);
        let fs = sim.fs();
        fs.create_root().unwrap();
        let lock = fs.lock().unwrap();
        let mut journal = opened(&sim);
        let mut written = Vec::new();
        for what in every_kind() {
            written.push(journal.record(&fs, what).unwrap());
        }
        let second = encode_record(&written[0]).unwrap().len() as u64;
        sim.damage(FILE, second + 40, 4).unwrap();
        drop(lock);
        sim.restart();
        sim
    };
    let (shown, found) = {
        let sim = build();
        let _lock = sim.fs().lock().unwrap();
        Journal::look(&sim.fs()).unwrap()
    };
    assert!(found.damaged_count > 0, "{found:?}");
    let through = shown[1].number;
    let after: Vec<Incident> = shown[2..].to_vec();
    let (mut accepted, mut refused) = (0, 0);
    for second in 0..64u64 {
        let sim = build();
        let fs = sim.fs();
        let lock = fs.lock().unwrap();
        let mut journal = opened(&sim);
        // The block read fails, and with `second` one read after it.
        let base = sim.steps();
        sim.place(base + 1, Fault::FailRead);
        if second > 0 {
            sim.place(base + 1 + second, Fault::FailRead);
        }
        let done = journal.acknowledge(&fs, through, &found);
        let fired = sim.fired().get(&Fault::FailRead).copied().unwrap_or(0);
        if fired < 1 + u64::from(second > 0) {
            // No read left to fail.
            break;
        }
        match &done {
            Ok(_) => accepted += 1,
            Err(JournalError::Refused(_)) => refused += 1,
            Err(e) => panic!("read {second}: {e}"),
        }
        drop(lock);
        sim.restart();
        let _lock = fs.lock().unwrap();
        let journal = opened(&sim);
        for incident in &after {
            assert!(
                journal.incidents().contains(incident),
                "read {second}, {done:?}: incident {} is gone: {:?}",
                incident.number,
                journal.incidents()
            );
        }
    }
    assert!(
        accepted > 0 && refused > 0,
        "{accepted} accepted, {refused} refused"
    );
}

/// A journal made, or made again, is named only once it holds a flushed
/// record: a crash at every step of an open that finds its journal lost
/// leaves no file named `incidents` that reads as a whole, empty journal;
/// the next open finds the record, or the journal lost again. A file of
/// that name that holds no record reads as lost.
#[test]
fn a_journal_made_again_is_named_only_once_it_holds_a_record() {
    let steps = {
        let sim = Sim::new(7);
        let fs = sim.fs();
        fs.create_root().unwrap();
        let _lock = fs.lock().unwrap();
        let mut journal = opened(&sim);
        let begin = sim.steps();
        journal.record(&fs, What::JournalLost).unwrap();
        sim.steps() - begin
    };
    for step in 1..=steps {
        for fates in [Fates::KeepAll, Fates::LoseAll, Fates::Drawn] {
            for seed in 0..8 {
                let sim = Sim::new(seed);
                let fs = sim.fs();
                fs.create_root().unwrap();
                let lock = fs.lock().unwrap();
                let mut journal = opened(&sim);
                assert!(journal.found().missing);
                sim.set_crash_fates(fates);
                sim.place(sim.steps() + step, Fault::Crash);
                assert!(journal.record(&fs, What::JournalLost).is_err());
                drop(lock);
                sim.restart();
                let _lock = fs.lock().unwrap();
                let journal = opened(&sim);
                // A file of that name holds a record.
                let named = fs.open(FILE, FileOptions::PLAIN).is_ok();
                assert!(
                    !named || !journal.found().missing,
                    "step {step}, {fates:?}, seed {seed}: `incidents` holds no record"
                );
                let lost = journal.found().missing
                    || journal
                        .incidents()
                        .iter()
                        .any(|i| i.what == What::JournalLost);
                assert!(
                    lost,
                    "step {step}, {fates:?}, seed {seed}: {:?}, {:?}",
                    journal.incidents(),
                    journal.found()
                );
            }
        }
    }
    // A name without a record: lost.
    let sim = Sim::new(8);
    let fs = sim.fs();
    fs.create_root().unwrap();
    let _lock = fs.lock().unwrap();
    drop(fs.create(FILE, FileOptions::PLAIN).unwrap());
    assert!(opened(&sim).found().missing);
    Journal::create(&fs, 0).unwrap();
    let journal = opened(&sim);
    assert!(journal.found().is_clean(), "{:?}", journal.found());
}

/// Whatever the page file can approve fits a record, at this build's
/// limits: every superblock slot's checkpoint given up, each with
/// [`MAX_RESTORE_POINTS`] names of the longest length, every slot written
/// over, the longest text. The record names [`NAMES_KEPT`] of each, counts
/// the rest, and carries the digest of the whole approval. A plan's run of
/// one fate takes one row; a plan that still cannot fit is refused before
/// anything is written.
#[test]
fn whatever_the_store_is_asked_to_give_up_can_be_recorded() {
    let sim = Sim::new(9);
    let fs = sim.fs();
    fs.create_root().unwrap();
    let _lock = fs.lock().unwrap();
    let mut journal = opened(&sim);
    let name = |i: usize| format!("{i:0width$}", width = MAX_RESTORE_NAME);
    let approval = GiveUp {
        checkpoints: (0..SLOTS as u64)
            .map(|c| GivenUp {
                checkpoint: CheckpointId {
                    number: u64::MAX - c,
                    meta: (u64::MAX, Hash([0xff; 32])),
                },
                lost: Lost::Points(
                    (0..MAX_RESTORE_POINTS)
                        .map(|i| (name(i), u64::MAX))
                        .collect(),
                ),
            })
            .collect(),
        slots: (0..SLOTS).collect(),
        onto: checkpoint(1),
    };
    let what = What::checkpoints(&approval, true, true, &"é".repeat(MAX_TEXT));
    let incident = journal.record(&fs, what.clone()).unwrap();
    let What::Checkpoints {
        approval: kept,
        left_out,
        digest,
        ..
    } = &incident.what
    else {
        panic!("{incident:?}")
    };
    assert_eq!(incident.what, what);
    let left = (MAX_RESTORE_POINTS - NAMES_KEPT) as u64;
    assert_eq!(left_out, &vec![left; SLOTS]);
    for given in &kept.checkpoints {
        assert!(matches!(&given.lost, Lost::Points(p) if p.len() == NAMES_KEPT));
    }
    let mut whole = Out::default();
    whole.give_up(&approval);
    assert_eq!(digest, &sha256(&whole.0));
    assert!(incident.to_string().contains("more restore points"));

    // A plan of the most segment files, one fate: one row.
    let plan = |files: Vec<FileFate>| {
        What::LogDamage(Box::new(DamagePlan {
            start: position(1),
            kept: position(1),
            marker: 1,
            cut_count_before: 0,
            cut_count: Some(1),
            listed_before: Segments {
                first: 1,
                count: files.len() as u64,
            },
            listed: Segments { first: 1, count: 1 },
            opens_at: position(1),
            files,
            tail_copied: None,
            before_start: None,
            prefix: Some("given-up-1.".into()),
        }))
    };
    let missing = |number: u64| FileFate {
        number,
        first: None,
        last: None,
        fate: Fate::Missing,
    };
    let most = plan((1..=MAX_FILES as u64).map(missing).collect());
    let recorded = journal.record(&fs, most.clone()).unwrap();
    assert_eq!(recorded.what, most);
    // Fates that change at every file: refused, nothing written.
    let alternating = plan(
        (1..=MAX_FILES as u64)
            .map(|n| FileFate {
                fate: if n % 2 == 0 {
                    Fate::Missing
                } else {
                    Fate::SetAside {
                        reason: SET_ASIDE_REASONS[0],
                    }
                },
                ..missing(n)
            })
            .collect(),
    );
    let before = journal.incidents().len();
    assert!(matches!(
        journal.record(&fs, alternating),
        Err(JournalError::TooLarge { .. })
    ));
    let too_many = plan((1..=MAX_FILES as u64 + 1).map(missing).collect());
    assert!(matches!(
        journal.record(&fs, too_many),
        Err(JournalError::TooLarge { .. })
    ));
    assert_eq!(journal.incidents().len(), before);
    sim.restart();
    assert_eq!(opened(&sim).incidents().len(), before);
}

/// The records of one act are written together or not at all; numbers are
/// checked: an open's run of numbers ends below the next start's, and a
/// start count whose numbers cannot be told is refused.
#[test]
fn an_act_is_recorded_whole_and_numbers_are_checked() {
    let sim = Sim::new(10);
    let fs = sim.fs();
    fs.create_root().unwrap();
    let _lock = fs.lock().unwrap();
    // The journal full but one: an act of two records writes neither.
    let mut journal = opened(&sim);
    for _ in 0..MAX_RECORDS - 1 {
        journal.record(&fs, What::JournalLost).unwrap();
    }
    assert!(matches!(
        journal.record_all(&fs, vec![What::JournalLost, What::JournalLost]),
        Err(JournalError::Full)
    ));
    sim.restart();
    let mut journal = opened(&sim);
    assert_eq!(journal.incidents().len(), MAX_RECORDS - 1);
    journal.record(&fs, What::JournalLost).unwrap();

    // Numbers: the last of start 3's run, then none.
    let start = 3;
    replace(&fs, &base_record((start + 1) * PER_START - 1, 0)).unwrap();
    let mut journal = Journal::open(&fs, start).unwrap();
    let last = journal.record(&fs, What::JournalLost).unwrap();
    assert_eq!(last.number, (start + 1) * PER_START - 1);
    assert!(matches!(
        journal.record(&fs, What::JournalLost),
        Err(JournalError::NumbersSpent)
    ));
    assert!(matches!(
        Journal::open(&fs, u64::MAX / PER_START + 1),
        Err(JournalError::NumbersSpent)
    ));
}
