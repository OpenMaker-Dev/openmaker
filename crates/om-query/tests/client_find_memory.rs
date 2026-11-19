//! A find on the client, for an index the host has not built yet
//! (`om_query::find::ClientFind`), keeps only the records it finds as the
//! table streams past, and for duplicates only each key's count and holders
//! within its budget: what it holds does not grow with the index it
//! replaces.
//!
//! The counting allocator is this binary's.

use std::alloc::System;
use std::collections::BTreeSet;

use om_collate::DateOrder;
use om_hbam::DataType;
use om_query::find::{self, ClientFind, ClientFindError};
use om_query::keys::{record_value_keys, word_keys, TextKeys};
use stats_alloc::{Region, StatsAlloc, INSTRUMENTED_SYSTEM};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const TABLE: u64 = 0x8001;
const FIELD: u64 = 1;
const MIB: isize = 1 << 20;

/// The kernel's high-water mark includes transient allocations during key
/// construction, map growth and final evaluation, not just retained state
/// sampled between records. This binary runs one test to isolate its peak.
#[cfg(target_os = "linux")]
fn peak_rss() -> usize {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse::<usize>()
        .unwrap()
        * 1024
}

fn finders(criterion: &str) -> Vec<Vec<u8>> {
    find::finders(
        TABLE,
        FIELD,
        DataType::Text,
        criterion,
        DateOrder::MonthDayYear,
    )
    .unwrap()
    .into_iter()
    .map(|f| f.bytes)
    .collect()
}

/// Streams `records` texts into a client find of `criterion`, keyed as the
/// value or word index (`word`) holds them, and returns the find's answer
/// (or the record it failed at) with the most bytes it held between
/// records.
fn stream(
    criterion: &str,
    budget: usize,
    records: u64,
    text: impl Fn(u64) -> String,
) -> (Result<Vec<u64>, (u64, ClientFindError)>, isize) {
    let region = Region::new(GLOBAL);
    // Reallocations count in both totals already.
    let held = |region: &Region<System>| {
        let change = region.change();
        change.bytes_allocated as isize - change.bytes_deallocated as isize
    };
    let mut client = ClientFind::new(&[finders(criterion)], budget).unwrap();
    let reads = client.reads().to_vec();
    let mut peak = 0;
    for id in 1..=records {
        let texts = [text(id)];
        let keys: Vec<BTreeSet<Vec<u8>>> = reads
            .iter()
            .map(|(_, index)| match index {
                find::IndexKind::Value => record_value_keys(
                    DataType::Text,
                    &texts,
                    DateOrder::MonthDayYear,
                    TextKeys::Collated,
                ),
                find::IndexKind::Word => word_keys(&texts, TextKeys::Collated),
            })
            .collect();
        if let Err(e) = client.add(id, &keys) {
            return (Err((id, e)), peak.max(held(&region)));
        }
        drop(keys);
        peak = peak.max(held(&region));
    }
    let found = client.finish().remove(0).iter().collect();
    (Ok(found), peak.max(held(&region)))
}

/// 100,000 records of five words each, every word distinct (500,000 keys
/// in the word index): a find that matches one record holds next to
/// nothing, where the index itself would take hundreds of MiB. Duplicates
/// keep a count and the holders of each key: few distinct values hold
/// little however many records there are, and distinct values beyond the
/// budget fail the find instead of growing past it. One test, since the
/// allocator counts every thread.
#[test]
fn client_finds_hold_their_answers_not_the_index() {
    let text = |id: u64| {
        (0..5)
            .map(|word| format!("word{word:02}record{id:08}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let (found, peak) = stream("word03record00054321", usize::MAX, 100_000, text);
    assert_eq!(found.unwrap(), [54_321]);
    assert!(peak < 4 * MIB, "word find: {peak} bytes held");
    eprintln!("500,000 distinct words: {peak} bytes held between records");
    #[cfg(target_os = "linux")]
    {
        let rss = peak_rss();
        eprintln!("word construction and evaluation peak RSS: {rss} bytes");
        assert!(rss < 48 * MIB as usize, "word find peak RSS: {rss}");
    }

    let few = |id: u64| format!("value {}", id % 10);
    let (found, peak) = stream("!", 64 * MIB as usize, 100_000, few);
    assert_eq!(found.unwrap().len(), 100_000);
    assert!(peak < 4 * MIB, "few duplicates: {peak} bytes held");
    eprintln!("100,000 records / 10 duplicate keys: {peak} bytes held");

    let budget = 4 * MIB as usize;
    let distinct = |id: u64| format!("value {id:08}");
    let (failed, peak) = stream("!", budget, 1_000_000, distinct);
    let (record, error) = failed.unwrap_err();
    assert!(
        matches!(error, ClientFindError::OverBudget { .. }),
        "{error}"
    );
    assert!(record < 100_000, "failed at record {record}");
    assert!(
        peak < 4 * budget as isize,
        "distinct duplicates: {peak} bytes held"
    );
    eprintln!("distinct duplicates: budget {budget}, failed at {record}, held {peak}");

    // A single input key larger than the budget must be rejected before
    // cloning it. Checking only after a record has been retained overshoots
    // by arbitrarily much, even though the between-record test above passes.
    let keys = [BTreeSet::from([vec![42; 8 * MIB as usize]])];
    let mut client = ClientFind::new(&[finders("!")], budget).unwrap();
    let region = Region::new(GLOBAL);
    assert!(matches!(
        client.add(1, &keys),
        Err(ClientFindError::OverBudget { .. })
    ));
    let allocated = region.change().bytes_allocated;
    assert!(
        allocated < MIB as usize,
        "oversized key cloned: {allocated}"
    );
    drop(keys);

    #[cfg(target_os = "linux")]
    {
        let rss = peak_rss();
        eprintln!("all construction, duplicate state and evaluation peak RSS: {rss} bytes");
        assert!(rss < 48 * MIB as usize, "client find peak RSS: {rss}");
    }
}
