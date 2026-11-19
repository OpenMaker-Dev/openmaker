//! Check both halves of the container codecs against the live runs against
//! FileMaker Server (`re/work/containers/live/c*`, git-ignored;
//! containers.md 11): every op87, op88 and op96 request parses and rebuilds
//! byte for byte, and every reply is rebuilt from its parsed form.
//!
//! Run: cargo test -p om-protocol --test container_vectors -- --ignored

use om_protocol::container;
use om_protocol::Opcode;
use om_rctransaction::Header;

const LIVE: &str = "../../re/work/containers/live";

#[test]
#[ignore = "requires the git-ignored container vectors"]
fn container_requests_and_replies_round_trip() {
    let mut checked = [0usize; 3];
    let mut runs: Vec<_> = std::fs::read_dir(LIVE)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    runs.sort();
    for run in runs {
        let mut requests: Vec<_> = std::fs::read_dir(&run)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "req"))
            .collect();
        requests.sort();
        for path in requests {
            let name = path.display().to_string();
            let req = std::fs::read(&path).unwrap();
            let rep = std::fs::read(path.with_extension("rep")).unwrap();
            let header = Header::parse(&req).unwrap();
            let (tid, file) = (header.transid_lo, header.file_id);
            match header.opcode {
                op if op == Opcode::UploadBinaryData as u8 => {
                    let parsed = container::parse_upload_binary_request(&req).unwrap();
                    assert_eq!(
                        container::build_upload_binary(tid, file, &parsed),
                        req,
                        "{name}"
                    );
                    let reply = container::parse_upload_binary_reply(&rep).unwrap();
                    assert_eq!(reply.to_result(), rep, "{name}");
                    checked[0] += 1;
                }
                op if op == Opcode::AdjustReferenceCount as u8 => {
                    // The truncated requests of run c2 (containers.md 10) are
                    // not well-formed op87 and are skipped.
                    let Ok(parsed) = container::parse_adjust_reference_count_request(&req) else {
                        assert!(req.len() < 30, "{name}");
                        continue;
                    };
                    assert_eq!(
                        container::build_adjust_reference_count(tid, file, &parsed),
                        req,
                        "{name}"
                    );
                    let count = container::parse_adjust_reference_count_reply(&rep).unwrap();
                    assert_eq!(
                        container::adjust_reference_count_reply(count),
                        rep,
                        "{name}"
                    );
                    checked[1] += 1;
                }
                op if op == Opcode::DownloadThumbnail as u8 => {
                    let parsed = container::parse_download_thumbnail_request(&req).unwrap();
                    assert_eq!(
                        container::build_download_thumbnail(tid, file, &parsed),
                        req,
                        "{name}"
                    );
                    let reply = container::parse_download_thumbnail_reply(&rep).unwrap();
                    assert_eq!(reply.to_result(), rep, "{name}");
                    checked[2] += 1;
                }
                _ => {}
            }
        }
    }
    println!(
        "op88 {}, op87 {}, op96 {}",
        checked[0], checked[1], checked[2]
    );
    assert!(checked.iter().all(|&n| n > 0), "{checked:?}");
}
