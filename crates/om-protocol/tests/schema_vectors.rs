//! Check the schema-editing codecs against the live runs against FileMaker
//! Server (`re/work/schema/live/s*`) and FileMaker Pro's own Manage Database
//! requests (`re/work/schema/capn`), both git-ignored (schema-path.md 11):
//! every op13, op15, op61, op73 and op82 request parses and rebuilds byte for
//! byte, and every FileMaker Server reply is rebuilt from its parsed form.
//!
//! Run: cargo test -p om-protocol --test schema_vectors -- --ignored

use std::path::{Path, PathBuf};

use om_protocol::schema;
use om_protocol::write::{self, LockReply};
use om_protocol::{
    build_download_from_host, parse_download_from_host_reply, parse_download_request,
    DownloadRequest, Opcode, Resume,
};
use om_rctransaction::Header;

const LIVE: &str = "../../re/work/schema/live";
const CAPTURE: &str = "../../re/work/schema/capn";

fn files(dir: &Path, keep: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut out: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| keep(p))
        .collect();
    out.sort();
    out
}

/// A request without the header's idle time (bytes 16-19), which FileMaker
/// Pro fills in and the builders leave 0.
fn sans_idle_time(transaction: &[u8]) -> Vec<u8> {
    [&transaction[..16], &transaction[20..]].concat()
}

/// Check one request and, when FileMaker Server answered it, its reply.
/// Returns whether the opcode is one of the schema ops.
fn check(name: &str, req: &[u8], rep: Option<&[u8]>) -> bool {
    let header = Header::parse(req).unwrap();
    let same =
        |rebuilt: Vec<u8>| assert_eq!(sans_idle_time(&rebuilt), sans_idle_time(req), "{name}");
    let (tid, file) = (header.transid_lo, header.file_id);
    match header.opcode {
        op if op == Opcode::HostTimeStamp as u8 => {
            same(schema::build_host_timestamp(tid, file));
            if let Some(rep) = rep {
                let key = schema::parse_host_timestamp_reply(rep).unwrap();
                assert_eq!(key.len(), 8, "{name}");
                assert_eq!(schema::host_timestamp_reply(&key), rep, "{name}");
            }
        }
        op if op == Opcode::Upgrade as u8 => {
            let parsed = schema::parse_upgrade_request(req).unwrap();
            same(schema::build_upgrade(tid, file, &parsed));
            if let Some(rep) = rep {
                let reply = write::parse_lock_reply(rep, false).unwrap();
                assert_eq!(reply, LockReply::Granted { mod_count: None }, "{name}");
                assert_eq!(reply.to_result().unwrap(), rep, "{name}");
            }
        }
        op if op == Opcode::DownloadFromHost as u8 => {
            let parsed = parse_download_request(req).unwrap();
            let path: Vec<&[u8]> = parsed.key_path.iter().map(Vec::as_slice).collect();
            let rebuilt = build_download_from_host(&DownloadRequest {
                transaction_id: tid,
                file_id: file,
                key_path: &path,
                resume: parsed.resume.clone(),
            });
            same(rebuilt);
            if let Some(rep) = rep {
                let reply = parse_download_from_host_reply(rep).unwrap();
                assert_eq!(reply.to_unlocked_result_within(usize::MAX), rep, "{name}");
            }
        }
        op if op == Opcode::UploadToHost as u8 => {
            let parsed = write::parse_upload_request(req).unwrap();
            assert_eq!(parsed.flags & write::UPLOAD_TO_TEMP, 0, "{name}");
            same(write::build_upload_to_host(tid, file, &parsed));
            if let Some(rep) = rep {
                assert_eq!(write::upload_reply(&Resume::no_stream(), usize::MAX), rep);
            }
        }
        op if op == Opcode::HostUpdateTable as u8 => {
            let table = schema::parse_update_table_request(req).unwrap();
            same(schema::build_update_table(tid, file, &table));
            if let Some(rep) = rep {
                assert_eq!(schema::update_table_reply(), rep, "{name}");
            }
        }
        _ => return false,
    }
    true
}

#[test]
#[ignore = "requires the git-ignored schema vectors"]
fn schema_requests_and_replies_round_trip() {
    let mut checked = 0;
    for run in files(Path::new(LIVE), |p| p.is_dir()) {
        for path in files(&run, |p| p.extension().is_some_and(|e| e == "req")) {
            let req = std::fs::read(&path).unwrap();
            let rep = std::fs::read(path.with_extension("rep")).unwrap();
            checked += usize::from(check(&path.display().to_string(), &req, Some(&rep)));
        }
    }
    // FileMaker Pro's requests; OpenMaker Server refused these ops then, so
    // the replies are not FileMaker Server's.
    for exchange in files(Path::new(CAPTURE), |p| p.is_dir()) {
        let req = std::fs::read(exchange.join("txn.bin")).unwrap();
        checked += usize::from(check(&exchange.display().to_string(), &req, None));
    }
    assert!(checked >= 30, "{checked} vectors");
    eprintln!("{checked} schema vectors");
}
