//! The incident journal against crashes and damage, on the simulated disk.

use super::*;
use crate::log::{FileFate, Position};
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
        What::Checkpoints {
            approval: GiveUp {
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
            damaged: true,
            by_administrator: false,
            why: "a checkpoint does not check".into(),
        },
        What::LogDamage(Box::new(DamagePlan {
            start: position(4),
            kept: position(9),
            marker: 12,
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
    assert_eq!(journal.acknowledge(&fs, 3).unwrap(), 3);
    assert_eq!(journal.incidents(), &written[3..]);
    let next = journal.record(&fs, What::JournalLost).unwrap();
    assert_eq!(next.number, 6);
    sim.restart();
    let mut journal = opened(&sim);
    assert_eq!(journal.incidents().len(), 3);
    assert_eq!(journal.incidents()[2], next);
    // All of them: none left, and numbers still go on.
    assert_eq!(journal.acknowledge(&fs, 6).unwrap(), 3);
    sim.restart();
    let mut journal = opened(&sim);
    assert!(journal.incidents().is_empty());
    assert_eq!(journal.record(&fs, What::JournalLost).unwrap().number, 7);
    assert!(matches!(
        journal.acknowledge(&fs, 8),
        Err(JournalError::Refused(_))
    ));
}

/// An incident of several sectors, so that a crash can tear it.
fn large() -> What {
    What::Checkpoints {
        approval: GiveUp {
            checkpoints: Vec::new(),
            slots: vec![2],
            onto: checkpoint(3),
        },
        damaged: false,
        by_administrator: true,
        why: "x".repeat(MAX_TEXT),
    }
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
    assert_eq!(journal.found().damaged, [second, last]);
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
    journal.acknowledge(&fs, first.number).unwrap();
    sim.damage(FILE, 40, 4).unwrap();
    sim.restart();
    let mut journal = Journal::open(&fs, 2).unwrap();
    assert!(!journal.found().damaged.is_empty());
    let next = journal.record(&fs, What::JournalLost).unwrap();
    assert!(next.number > first.number, "{next:?} after {first:?}");
}
