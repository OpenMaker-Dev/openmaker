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

/// A plan of `files`, as an acceptance would make it.
fn plan_of(files: Vec<FileFate>) -> DamagePlan {
    DamagePlan {
        start: position(4),
        kept: position(9),
        marker: 12,
        cut_count_before: 1,
        cut_count: Some(2),
        listed_before: Segments {
            first: 1,
            count: files.len() as u64,
        },
        listed: Segments { first: 2, count: 2 },
        opens_at: position(9),
        files,
        tail_copied: Some(3),
        before_start: Some(ChainFault {
            segment: 2,
            offset: 300,
            index: 3,
            unreadable: true,
        }),
        prefix: Some("given-up-2.".into()),
    }
}

fn file(number: u64, first: Option<u64>, last: Option<u64>, fate: Fate) -> FileFate {
    FileFate {
        number,
        first,
        last,
        fate,
    }
}

/// Every fate, and a run of three missing segments (one row).
fn every_fate() -> DamagePlan {
    let set_aside = Fate::SetAside {
        reason: SET_ASIDE_REASONS[1],
    };
    plan_of(vec![
        file(1, Some(1), Some(3), set_aside),
        file(2, Some(4), None, Fate::Lengthened { size: 4096 }),
        file(3, None, None, Fate::Kept),
        file(4, None, Some(20), Fate::Missing),
        file(
            5,
            Some(21),
            Some(30),
            Fate::MovedEarlier {
                to: "log/given-up-1.0000000000000005".into(),
            },
        ),
        file(6, Some(31), None, Fate::Missing),
        file(7, None, None, Fate::Missing),
        file(8, None, Some(40), Fate::Missing),
    ])
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
        What::LogDamage {
            plan: Box::new(PlanRecord::of(&every_fate()).unwrap()),
            last_batch_after: Some(12),
        },
        What::UnreadableEnd {
            last: position(30),
            end: UnreadableEnd {
                segment: 4,
                offset: 900,
                index: 31,
            },
        },
        What::LastBatch { after: 40 },
        What::JournalLost,
    ]
}

/// The journal, as the closed store's calls see it (start count 0, so
/// numbers from 1).
fn opened(sim: &Sim) -> Journal {
    Journal::open(&sim.fs(), 0).unwrap()
}

/// Every kind reads back as it was written, in order, numbered from 1; a
/// plan's run stays one row; acknowledging what a read showed leaves only a
/// base record, and numbers go on after it; a read made before a record was
/// added is refused.
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
    let mut journal = opened(&sim);
    assert!(journal.found().is_clean(), "{:?}", journal.found());
    assert_eq!(journal.incidents(), &written[..]);
    let What::LogDamage { plan, .. } = &journal.incidents()[1].what else {
        panic!()
    };
    assert_eq!(plan.rows.len(), 6, "{:?}", plan.rows);
    assert_eq!(plan.files().collect::<Vec<_>>(), every_fate().files);
    let reading = look(&fs).unwrap();
    assert_eq!(reading.incidents, written);
    assert_eq!(journal.acknowledge(&fs, &reading.shown).unwrap(), 5);
    assert!(journal.incidents().is_empty());
    let next = journal.record(&fs, What::JournalLost).unwrap();
    assert_eq!(next.number, 6);
    sim.restart();
    let mut journal = opened(&sim);
    assert_eq!(journal.incidents(), std::slice::from_ref(&next));
    assert!(journal.found().is_clean());
    // What a read showed, with a record added since: refused.
    let stale = look(&fs).unwrap();
    journal.record(&fs, What::JournalLost).unwrap();
    assert!(matches!(
        journal.acknowledge(&fs, &stale.shown),
        Err(JournalError::Refused(_))
    ));
    assert_eq!(journal.incidents().len(), 2);
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

/// A crash at every step of an append, each settled three ways, on every
/// schedule: the records before it are read whole; the one cut short is
/// read whole, as written, or any of its bytes that reached the file are
/// reported as a place that does not check after the last record; the
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
        let end = journal.end;
        drop(lock);
        sim.restart();
        (sim, before, end)
    };
    let len = encode_record(&Incident {
        number: 3,
        start_count: 0,
        what: large(),
    })
    .unwrap()
    .len();
    let steps = {
        let (sim, _, _) = build(0);
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
                let case = format!("step {step}, {fates:?}, seed {seed}");
                let (sim, before, end) = build(seed);
                let fs = sim.fs();
                let lock = fs.lock().unwrap();
                let mut journal = opened(&sim);
                sim.set_crash_fates(fates);
                sim.place(sim.steps() + step, Fault::Crash);
                let cut_short = journal.record(&fs, large()).is_err();
                assert!(cut_short, "{case}: the crash fired");
                drop(lock);
                sim.restart();
                let _lock = fs.lock().unwrap();
                let mut journal = opened(&sim);
                let read = journal.incidents().to_vec();
                assert_eq!(&read[..2], &before[..], "{case}");
                assert!(read.len() <= 3, "{case}");
                let found = journal.found().clone();
                match read.get(2) {
                    // Read whole, it is what was written, and nothing else.
                    Some(third) => assert_eq!(
                        third,
                        &Incident {
                            number: 3,
                            start_count: 0,
                            what: large(),
                        },
                        "{case}"
                    ),
                    // Whatever of it reached the file is reported.
                    None => {
                        let bytes = sim.visible(FILE).unwrap_or_default();
                        let from = (end as usize).min(bytes.len());
                        let to = (end as usize + len).min(bytes.len());
                        if bytes[from..to].iter().any(|&b| b != 0) {
                            torn += 1;
                            assert!(found.ends_damaged, "{case}: {found:?}");
                        } else {
                            assert!(found.is_clean(), "{case}: {found:?}");
                        }
                    }
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
    // The block read, then each of its sectors, fails.
    let sectors = encode_record(&written[0]).unwrap().len() / ALIGN as usize
        + encode_record(&written[1]).unwrap().len() / ALIGN as usize;
    for _ in 0..1 + sectors {
        sim.place(sim.steps() + 1, Fault::FailRead);
    }
    let mut journal = opened(&sim);
    assert!(journal.incidents().is_empty());
    assert!(journal.found().missing);
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

/// Numbers are checked. After an acknowledgement the journal's only record
/// of the numbers used is its base record: damaged, the next start's
/// numbers still come after every earlier one; unreadable, an open in the
/// same start (which the store never makes) still skips past it. An open's
/// numbers end below the next start's, and a start count whose numbers do
/// not fit is refused.
#[test]
fn numbers_never_come_again_and_are_checked() {
    let sim = Sim::new(5);
    let fs = sim.fs();
    fs.create_root().unwrap();
    let _lock = fs.lock().unwrap();
    let mut journal = Journal::open(&fs, 1).unwrap();
    let first = journal.record(&fs, What::JournalLost).unwrap();
    assert_eq!(first.number, PER_START);
    journal.acknowledge(&fs, &look(&fs).unwrap().shown).unwrap();
    sim.damage(FILE, 40, 4).unwrap();
    sim.restart();
    let mut journal = Journal::open(&fs, 2).unwrap();
    assert!(!journal.found().damaged.is_empty());
    let next = journal.record(&fs, What::JournalLost).unwrap();
    assert!(next.number > first.number, "{next:?} after {first:?}");

    // The base record unreadable, the journal opened again in start 1.
    replace(&fs, &base_record(PER_START + 1, 1)).unwrap();
    sim.restart();
    sim.place(sim.steps() + 1, Fault::FailRead);
    sim.place(sim.steps() + 1, Fault::FailRead);
    let mut journal = Journal::open(&fs, 1).unwrap();
    assert!(!journal.found().unreadable.is_empty());
    let again = journal.record(&fs, What::JournalLost).unwrap();
    assert!(again.number > first.number, "{again:?} after {first:?}");

    // The last of start 3's numbers, then none.
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

/// Round 2 of #766, Sol's first finding: an acknowledgement acknowledges
/// everything a read showed, or nothing. Forty records, twenty of them
/// damaged (more than a read lists), then one change between the read and
/// the call: a record added, a damaged place past the first sixteen whose
/// bytes change or become zeros, a sector that stops reading, a sector that
/// starts reading. Each refuses the call, and a clean read then shows every
/// record a clean read showed before. A block read that fails and reads
/// again whole, sector by sector, changes nothing, and is accepted.
#[test]
fn an_acknowledgement_is_refused_when_the_journal_changed_since_it_was_read() {
    const RECORDS: u64 = 40;
    let build = || {
        let sim = Sim::new(6);
        let fs = sim.fs();
        fs.create_root().unwrap();
        let lock = fs.lock().unwrap();
        let mut journal = opened(&sim);
        for _ in 0..RECORDS {
            journal.record(&fs, What::JournalLost).unwrap();
        }
        for n in 2..22 {
            sim.damage(FILE, n * ALIGN + 40, 4).unwrap();
        }
        drop(lock);
        sim.restart();
        sim
    };
    let clean = {
        let sim = build();
        let _lock = sim.fs().lock().unwrap();
        look(&sim.fs()).unwrap()
    };
    assert_eq!(clean.found.damaged_count, 20, "{:?}", clean.found);
    assert_eq!(clean.found.damaged.len(), MAX_REPORTED);
    let seventeenth = clean.found.damaged[MAX_REPORTED - 1] + 2 * ALIGN;
    // Refused, and nothing lost: what a clean read shows after it.
    let refused = |sim: &Sim, done: Result<usize, JournalError>, added: usize, what: &str| {
        assert!(
            matches!(done, Err(JournalError::Refused(_))),
            "{what}: {done:?}"
        );
        sim.restart();
        let _lock = sim.fs().lock().unwrap();
        let after = look(&sim.fs()).unwrap();
        assert_eq!(
            after.incidents[..clean.incidents.len()],
            clean.incidents[..],
            "{what}"
        );
        assert_eq!(
            after.incidents.len(),
            clean.incidents.len() + added,
            "{what}"
        );
    };
    // A record added.
    {
        let sim = build();
        let fs = sim.fs();
        let _lock = fs.lock().unwrap();
        let shown = look(&fs).unwrap().shown;
        let mut journal = opened(&sim);
        journal.record(&fs, What::JournalLost).unwrap();
        let done = journal.acknowledge(&fs, &shown);
        drop(_lock);
        refused(&sim, done, 1, "a record added");
    }
    // A damaged place past the first sixteen changes, or reads zeros.
    for zeros in [false, true] {
        let sim = build();
        let fs = sim.fs();
        let lock = fs.lock().unwrap();
        let shown = look(&fs).unwrap().shown;
        if zeros {
            let file = fs.open(FILE, FileOptions::PLAIN).unwrap();
            file.write_at(seventeenth, &[0; ALIGN as usize]).unwrap();
            file.flush().unwrap();
        } else {
            sim.damage(FILE, seventeenth + 100, 4).unwrap();
        }
        let done = acknowledge_closed(&fs, &shown);
        drop(lock);
        refused(
            &sim,
            done,
            0,
            &format!("a damaged place changed, zeros {zeros}"),
        );
    }
    // A sector that stops reading during the call, or that did not read
    // when the journal was shown and reads now: each sector in turn (the
    // block read fails, then the sector's own).
    let (mut stops, mut starts) = (0, 0);
    for during_call in [true, false] {
        for sector in 1..=RECORDS + 4 {
            let sim = build();
            let fs = sim.fs();
            let lock = fs.lock().unwrap();
            let fail = |sim: &Sim| {
                let base = sim.steps();
                sim.place(base + 1, Fault::FailRead);
                sim.place(base + 2 + sector, Fault::FailRead);
            };
            let shown = if during_call {
                let shown = look(&fs).unwrap().shown;
                fail(&sim);
                shown
            } else {
                fail(&sim);
                look(&fs).unwrap().shown
            };
            let fired = |sim: &Sim| sim.fired().get(&Fault::FailRead).copied().unwrap_or(0);
            if !during_call && fired(&sim) < 2 {
                break;
            }
            let done = acknowledge_closed(&fs, &shown);
            if during_call && fired(&sim) < 2 {
                break;
            }
            drop(lock);
            if during_call {
                stops += 1;
            } else {
                starts += 1;
            }
            refused(
                &sim,
                done,
                0,
                &format!("sector {sector}, during the call {during_call}"),
            );
        }
    }
    assert!(
        stops > RECORDS / 2 && starts > RECORDS / 2,
        "{stops} {starts}"
    );
    // The block read fails during the call and reads whole sector by
    // sector: the same bytes, accepted.
    let sim = build();
    let fs = sim.fs();
    let _lock = fs.lock().unwrap();
    let shown = look(&fs).unwrap().shown;
    sim.place(sim.steps() + 1, Fault::FailRead);
    assert_eq!(
        acknowledge_closed(&fs, &shown).unwrap(),
        clean.incidents.len()
    );
    assert!(look(&fs).unwrap().incidents.is_empty());
}

/// Round 2 of #766, Sol's fourth finding: a named journal that holds no
/// record and has no room left is lost, and the record of that is refused
/// as full; reading it and acknowledging what the read showed is the way
/// forward, and the journal then records again.
#[test]
fn a_lost_journal_with_no_room_is_acknowledged_away() {
    let sim = Sim::new(11);
    let fs = sim.fs();
    fs.create_root().unwrap();
    let _lock = fs.lock().unwrap();
    let file = fs.create(FILE, FileOptions::PLAIN).unwrap();
    file.write_at(0, &vec![0x5a; MAX_FILE as usize]).unwrap();
    file.flush().unwrap();
    drop(file);
    let mut journal = opened(&sim);
    assert!(journal.found().missing);
    let full = journal.record(&fs, What::JournalLost).unwrap_err();
    assert!(matches!(full, JournalError::Full), "{full}");
    assert!(full.to_string().contains("acknowledges"), "{full}");
    let reading = look(&fs).unwrap();
    assert!(reading.found.missing && reading.found.damaged_count > 0);
    assert_eq!(acknowledge_closed(&fs, &reading.shown).unwrap(), 0);
    let mut journal = opened(&sim);
    assert!(journal.found().is_clean(), "{:?}", journal.found());
    journal.record(&fs, What::JournalLost).unwrap();
}

/// A journal made, or made again, is named only once it holds a flushed
/// record: a crash at every step of an open that finds its journal lost
/// leaves no file named `incidents` that reads as a whole, empty journal;
/// the next open finds the record, or the journal lost again. The same from
/// a named file that holds no record that checks (the record goes after
/// what it holds). A file of that name that holds no record reads as lost.
#[test]
fn a_journal_made_again_is_named_only_once_it_holds_a_record() {
    // `named`: a file named `incidents` holds bytes that are no record.
    let build = |seed: u64, named: bool| {
        let sim = Sim::new(seed);
        let fs = sim.fs();
        fs.create_root().unwrap();
        if named {
            let file = fs.create(FILE, FileOptions::PLAIN).unwrap();
            file.write_at(0, &[0x5a; 3 * ALIGN as usize]).unwrap();
            file.flush().unwrap();
            fs.flush_folder("").unwrap();
        }
        sim.restart();
        sim
    };
    for named in [false, true] {
        let steps = {
            let sim = build(7, named);
            let fs = sim.fs();
            let _lock = fs.lock().unwrap();
            let mut journal = opened(&sim);
            assert!(journal.found().missing);
            let begin = sim.steps();
            journal.record(&fs, What::JournalLost).unwrap();
            sim.steps() - begin
        };
        for step in 1..=steps {
            for fates in [Fates::KeepAll, Fates::LoseAll, Fates::Drawn] {
                for seed in 0..8 {
                    let case = format!("named {named}, step {step}, {fates:?}, seed {seed}");
                    let sim = build(seed, named);
                    let fs = sim.fs();
                    let lock = fs.lock().unwrap();
                    let mut journal = opened(&sim);
                    sim.set_crash_fates(fates);
                    sim.place(sim.steps() + step, Fault::Crash);
                    assert!(journal.record(&fs, What::JournalLost).is_err(), "{case}");
                    drop(lock);
                    sim.restart();
                    let _lock = fs.lock().unwrap();
                    let journal = opened(&sim);
                    // A file it named holds a record.
                    let present = fs.open(FILE, FileOptions::PLAIN).is_ok();
                    assert!(
                        named || !present || !journal.found().missing,
                        "{case}: `incidents` holds no record"
                    );
                    let lost = journal.found().missing
                        || journal
                            .incidents()
                            .iter()
                            .any(|i| i.what == What::JournalLost);
                    assert!(
                        lost,
                        "{case}: {:?}, {:?}",
                        journal.incidents(),
                        journal.found()
                    );
                }
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
/// one fate takes one row, and stays one row when read: a journal full of
/// plans of the most files decodes to a row each (Sol's fifth finding of
/// round 2). A plan that still cannot fit is refused before anything is
/// written.
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
    for (given, whole) in kept.checkpoints.iter().zip(&approval.checkpoints) {
        let (Lost::Points(p), Lost::Points(all)) = (&given.lost, &whole.lost) else {
            panic!()
        };
        assert_eq!(p[..], all[..NAMES_KEPT]);
    }
    let mut whole = Out::default();
    whole.give_up(&approval);
    assert_eq!(digest, &sha256(&whole.0));
    assert_ne!(
        digest,
        &sha256(&{
            let mut kept_only = Out::default();
            kept_only.give_up(kept);
            kept_only.0
        })
    );
    assert!(incident.to_string().contains("more restore points"));

    // Plans of the most segment files, one fate: one row each, read back
    // as one row each.
    let missing = |number: u64| file(number, None, None, Fate::Missing);
    let most = What::LogDamage {
        plan: Box::new(
            PlanRecord::of(&plan_of((1..=MAX_FILES as u64).map(missing).collect())).unwrap(),
        ),
        last_batch_after: None,
    };
    while journal.incidents().len() < MAX_RECORDS {
        journal.record(&fs, most.clone()).unwrap();
    }
    // Fates that change at every file: refused, nothing written; more files
    // than a record holds: refused when made.
    let alternating: Vec<FileFate> = (1..=MAX_FILES as u64)
        .map(|n| {
            let fate = if n % 2 == 0 {
                Fate::Missing
            } else {
                Fate::SetAside {
                    reason: SET_ASIDE_REASONS[0],
                }
            };
            file(n, None, None, fate)
        })
        .collect();
    let mut room = opened(&sim);
    room.incidents.clear();
    assert!(matches!(
        room.record(
            &fs,
            What::LogDamage {
                plan: Box::new(PlanRecord::of(&plan_of(alternating)).unwrap()),
                last_batch_after: None,
            }
        ),
        Err(JournalError::TooLarge { .. })
    ));
    assert!(matches!(
        PlanRecord::of(&plan_of((1..=MAX_FILES as u64 + 1).map(missing).collect())),
        Err(JournalError::TooLarge { .. })
    ));
    sim.restart();
    let journal = opened(&sim);
    assert_eq!(journal.incidents().len(), MAX_RECORDS);
    let (mut rows, mut files) = (0, 0u64);
    for incident in journal.incidents() {
        if let What::LogDamage { plan, .. } = &incident.what {
            rows += plan.rows.len();
            files += plan.rows.iter().map(|r| u64::from(r.count)).sum::<u64>();
        }
    }
    assert_eq!(rows, MAX_RECORDS - 1);
    assert_eq!(files, (MAX_RECORDS as u64 - 1) * MAX_FILES as u64);
}
