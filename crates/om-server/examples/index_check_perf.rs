//! How often a commit checks an index against its records, and what that
//! costs: op70 commits that change the indexed Name of 1 and of 1,000
//! records of a table of 100,000, the first after the file is loaded and
//! the ones after it, over loopback TLS, each save included; the
//! measurement of docs/performance/index-checks.md.
//! cargo run --release -p om-server --example index_check_perf
//!
//! The file is the tests' Tasks file (`tests/fixtures/tasks.rs`) with
//! records 3 to 100,000 added and a value index on Name that agrees with
//! them. Each commit's time is printed; each check of an index against its
//! records is logged by the server (`Store::index_agrees`, debug) on
//! stderr.

#[path = "../tests/fixtures/tasks.rs"]
mod fixture;
#[path = "../tests/fixtures/server.rs"]
mod server;

use std::collections::BTreeMap;
use std::time::Instant;

use om_hbam::slot::uint_segment;
use om_hbam::{serial, OwnedNode, RidList};

const RECORDS: u64 = 100_000;
const TXN: u32 = 0x31;

fn main() {
    env_logger::Builder::new()
        .filter_module("om_server::store", log::LevelFilter::Debug)
        .init();
    let mut root = fixture::tree(&[], true);
    fixture::add_tasks(&mut root, 3..=RECORDS);
    let table = uint_segment(fixture::TABLE);
    let name = uint_segment(fixture::NAME);
    // Name's value index as the records give it, and its counts.
    let mut keys: BTreeMap<Vec<u8>, RidList> = BTreeMap::new();
    let names = [(1, "Write tests".to_string()), (2, "Ship".to_string())]
        .into_iter()
        .chain((3..=RECORDS).map(|id| (id, format!("Task {id}"))));
    for (id, text) in names {
        keys.entry(om_collate::text_key(&text))
            .or_default()
            .insert(id);
    }
    let (entries, distinct) = (keys.values().map(RidList::len).sum::<usize>(), keys.len());
    let index = OwnedNode::list(
        vec![0],
        keys.into_iter()
            .map(|(key, ids)| ids.to_node(key))
            .collect(),
    );
    root.put(&[&table, &[1], &name, &[0]], index);
    let mut counts = (entries as u64).to_be_bytes().to_vec();
    counts.extend((distinct as u64).to_be_bytes());
    root.put(
        &[&table, &[1], &name, &[0x10]],
        OwnedNode::leaf(Vec::new(), counts),
    );
    let file = fixture::TempFile::from_tree("index-check-perf", &root);
    drop(root);
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "index-check-perf");
    let mut next = 3;
    println!("commit,committed,ms");
    let mut commit = |label: &str, count: u64| {
        let ids = next..next + count;
        next += count;
        let records = ids
            .clone()
            .map(|id| {
                OwnedNode::list(
                    serial::encode(id),
                    vec![OwnedNode::leaf(
                        name.clone(),
                        om_scsu::encode_scsu(&format!("Renamed {id}")),
                    )],
                )
            })
            .collect();
        let entries = ids
            .map(|id| {
                OwnedNode::list(
                    serial::encode(id),
                    vec![OwnedNode::leaf(name.clone(), Vec::new())],
                )
            })
            .collect();
        client.upload_records(&table, records).unwrap();
        eprintln!("-- commit {label}");
        let start = Instant::now();
        client.commit_records(&table, entries, TXN).unwrap();
        println!(
            "{label},{count},{:.3}",
            start.elapsed().as_secs_f64() * 1000.0
        );
    };
    commit("first after the load", 1);
    for n in 2..=8 {
        commit(&format!("single {n}"), 1);
    }
    for n in 1..=3 {
        commit(&format!("thousand {n}"), 1_000);
    }
    let records = server::records_of(&mut client, fixture::TABLE);
    assert_eq!(
        server::cell(&records, next - 1, fixture::NAME),
        Some(format!("Renamed {}", next - 1).as_str())
    );
    client.disconnect().unwrap();
}
