//! The time of an op70 commit of 1 and of 1,000 changed records into a
//! table of 100,000, over loopback TLS, the save of the file included: the
//! measurement of docs/performance/commit-through-transaction.md.
//! cargo run --release -p om-server --example commit_perf
//!
//! The file is the tests' Tasks file (`tests/fixtures/tasks.rs`) with
//! records 3 to 100,000 added; each sample changes the Name of a block of
//! records no earlier sample changed, uploads them (op14) and times the
//! commit (op70) alone. The first sample of each size is a warm-up.

#[path = "../tests/fixtures/tasks.rs"]
mod fixture;
#[path = "../tests/fixtures/server.rs"]
mod server;

use std::time::Instant;

use om_hbam::slot::uint_segment;
use om_hbam::{serial, OwnedNode};

const RECORDS: u64 = 100_000;
const SAMPLES: usize = 7;
const TXN: u32 = 0x31;

fn main() {
    let mut root = fixture::tree(&[], true);
    fixture::add_tasks(&mut root, 3..=RECORDS);
    let file = fixture::TempFile::from_tree("commit-perf", &root);
    drop(root);
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "commit-perf");
    let table = uint_segment(fixture::TABLE);
    let name = uint_segment(fixture::NAME);
    let mut next = 1;
    println!("records,committed,median_ms,samples_ms");
    for count in [1u64, 1_000] {
        let mut samples = Vec::with_capacity(SAMPLES);
        for sample in 0..=SAMPLES {
            let ids = next..next + count;
            next += count;
            let text = format!("Changed {count} {sample}");
            let records = ids
                .clone()
                .map(|id| {
                    OwnedNode::list(
                        serial::encode(id),
                        vec![OwnedNode::leaf(name.clone(), om_scsu::encode_scsu(&text))],
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
            let start = Instant::now();
            client.commit_records(&table, entries, TXN).unwrap();
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            if sample > 0 {
                samples.push(ms);
            }
        }
        let mut sorted = samples.clone();
        sorted.sort_by(f64::total_cmp);
        let samples: Vec<String> = samples.iter().map(|ms| format!("{ms:.3}")).collect();
        println!(
            "{RECORDS},{count},{:.3},{}",
            sorted[SAMPLES / 2],
            samples.join(" ")
        );
    }
    // The last change is stored: it reads back.
    let records = server::records_of(&mut client, fixture::TABLE);
    let last = next - 1;
    assert_eq!(
        server::cell(&records, last, fixture::NAME),
        Some(format!("Changed 1000 {SAMPLES}").as_str())
    );
    client.disconnect().unwrap();
}
