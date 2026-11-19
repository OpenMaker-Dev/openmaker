//! Run: cargo bench -p om-hbam --bench rebuild
//! Optional HBAM_BENCH_OUTPUT directory receives every complete output file,
//! so baseline and candidate runs can be compared byte for byte with `cmp`.
//! Optional HBAM_BENCH_INPUT adds a real local FileMaker file as a workload.
//! Construction, output comparison and parsing are outside the timed region.
use std::hint::black_box;
use std::time::Instant;

use om_hbam::{serial, writer, HbamFile, OwnedNode};

fn main() {
    let output = std::env::var_os("HBAM_BENCH_OUTPUT").map(std::path::PathBuf::from);
    if let Some(path) = &output {
        std::fs::create_dir_all(path).unwrap();
    }
    let mut superblock = vec![0u8; 4096];
    superblock[0x0f..0x14].copy_from_slice(om_hbam::container::MAGIC);
    superblock[0x14..0x18].copy_from_slice(&4096u32.to_be_bytes());
    println!("fixture,bytes,sample,elapsed_ns");
    for count in [1_000u64, 10_000, 50_000] {
        let records = (1..=count)
            .map(|id| {
                let cells = (1..=8u8)
                    .map(|field| OwnedNode::leaf(vec![field], vec![b'A' ^ 0x5a; 64]))
                    .collect();
                OwnedNode::list(serial::encode(id), cells)
            })
            .collect();
        let root = OwnedNode::list(
            Vec::new(),
            vec![OwnedNode::list(
                vec![0x80, 0x01],
                vec![OwnedNode::list(vec![5], records)],
            )],
        );
        let expected = writer::rebuild(&root, &superblock).unwrap();
        let file = HbamFile::from_bytes(expected.clone()).unwrap();
        let parsed = file.records(0x8001).unwrap();
        assert_eq!(parsed.len(), count as usize);
        for (index, record) in parsed.iter().enumerate() {
            assert_eq!(record.id, index as u64 + 1);
            assert_eq!(record.cells.len(), 8);
            for field in 1..=8 {
                assert_eq!(record.text(field).unwrap().unwrap(), "A".repeat(64));
            }
        }
        measure(
            &count.to_string(),
            &root,
            &superblock,
            &expected,
            output.as_deref(),
        );
    }
    if let Some(path) = std::env::var_os("HBAM_BENCH_INPUT") {
        let file = HbamFile::open(path).unwrap();
        let root = file.key_tree().unwrap().root.to_owned_node();
        let expected = writer::rebuild(&root, file.superblock().raw).unwrap();
        let rebuilt = HbamFile::from_bytes(expected.clone()).unwrap();
        assert_eq!(rebuilt.key_tree().unwrap().root.to_owned_node(), root);
        measure(
            "file",
            &root,
            file.superblock().raw,
            &expected,
            output.as_deref(),
        );
    }
}

fn measure(
    name: &str,
    root: &OwnedNode,
    superblock: &[u8],
    expected: &[u8],
    output: Option<&std::path::Path>,
) {
    if let Some(path) = output {
        std::fs::write(path.join(format!("{name}.fmp12")), expected).unwrap();
    }
    for sample in 0..7 {
        let start = Instant::now();
        let actual = writer::rebuild(black_box(root), black_box(superblock)).unwrap();
        let elapsed = start.elapsed();
        assert_eq!(actual, expected);
        println!("{name},{},{sample},{}", actual.len(), elapsed.as_nanos());
        black_box(actual);
    }
}
