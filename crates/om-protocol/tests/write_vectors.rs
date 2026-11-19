//! Check both halves of the write-path codecs against the vectors recorded
//! from FileMaker Pro (capture s03) and the live write runs against FileMaker
//! Server (`re/work/write/vectors/`, git-ignored; write-path.md 10): every
//! request parses and rebuilds byte for byte, and every reply is rebuilt
//! from its parsed form.
//!
//! Run: cargo test -p om-protocol --test write_vectors -- --ignored

use std::path::Path;

use om_protocol::write::{self, LockReply};
use om_protocol::{parse_request_notification, Opcode, Resume};
use om_rctransaction::Header;

const VECTORS: &str = "../../re/work/write/vectors";

/// One vector file: `req`, `rep` and `bc` lines in hex.
struct Vector {
    name: String,
    req: Option<Vec<u8>>,
    rep: Option<Vec<u8>>,
    bc: Option<Vec<u8>>,
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn vectors() -> Vec<Vector> {
    let mut out = Vec::new();
    let mut paths: Vec<_> = std::fs::read_dir(VECTORS)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "txt"))
        .collect();
    paths.sort();
    for path in paths {
        let text = std::fs::read_to_string(&path).unwrap();
        let line = |tag: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(&format!("{tag} ")))
                .map(|h| hex(h.trim()))
        };
        out.push(Vector {
            name: Path::new(&path)
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            req: line("req"),
            rep: line("rep"),
            bc: line("bc"),
        });
    }
    out
}

#[test]
#[ignore = "requires the git-ignored write-path vectors"]
fn requests_and_replies_round_trip() {
    let mut checked = 0;
    for v in vectors() {
        let name = &v.name;
        if let Some(bc) = &v.bc {
            if bc.starts_with(&[0xff, 0x00]) {
                let notifications = write::parse_notification_backchannel(bc)
                    .unwrap_or_else(|| panic!("{name}: backchannel"));
                assert_eq!(
                    write::notification_backchannel(&notifications),
                    *bc,
                    "{name}"
                );
                checked += 1;
            }
        }
        // That probe request carried one extra byte (a 5-byte transaction
        // id), so FileMaker Server read misaligned fields (write-path.md 4.2).
        if name == "live-op60-unexplained-empty-grant" {
            continue;
        }
        let Some(req) = &v.req else { continue };
        let header = Header::parse(req).unwrap();
        let (tid, file) = (header.transid_lo, header.file_id);
        let rep = v.rep.as_deref();
        match header.opcode {
            op if op == Opcode::HostCreateRecord as u8 => {
                let parsed = write::parse_create_record_request(req).unwrap();
                assert_eq!(
                    write::build_create_record(tid, file, &parsed),
                    *req,
                    "{name}"
                );
                let reply = write::parse_create_record_reply(rep.unwrap()).unwrap();
                assert_eq!(reply.to_result(), rep.unwrap(), "{name}");
            }
            op if op == Opcode::HostGetSerial as u8 => {
                let parsed = write::parse_get_serial_request(req).unwrap();
                assert_eq!(write::build_get_serial(tid, file, &parsed), *req, "{name}");
                let reply = write::parse_get_serial_reply(rep.unwrap()).unwrap();
                assert_eq!(reply.to_result(), rep.unwrap(), "{name}");
            }
            op if op == Opcode::UploadListToHostTempLocation as u8 => {
                let parsed = write::parse_upload_request(req).unwrap();
                assert_eq!(parsed.flags, write::UPLOAD_FLAGS, "{name}");
                assert_eq!(write::build_upload(tid, file, &parsed), *req, "{name}");
                assert_eq!(
                    write::upload_reply(&Resume::no_stream(), usize::MAX),
                    rep.unwrap(),
                    "{name}"
                );
            }
            op if op == Opcode::HostCommit as u8 => {
                let parsed = write::parse_commit_request(req).unwrap();
                assert_eq!(write::build_commit(tid, file, &parsed), *req, "{name}");
                assert_eq!(
                    write::upload_reply(&Resume::no_stream(), usize::MAX),
                    rep.unwrap(),
                    "{name}"
                );
            }
            op if op == Opcode::TransactInfo as u8 => {
                let parsed = write::parse_transact_info_request(req).unwrap();
                assert_eq!(
                    write::build_transact_info(tid, file, &parsed),
                    *req,
                    "{name}"
                );
                assert_eq!(
                    rep.unwrap(),
                    write::TRANSACT_INFO_REPLY.to_be_bytes(),
                    "{name}"
                );
            }
            op if op == Opcode::Lock as u8 => {
                let parsed =
                    write::parse_lock_request(req).unwrap_or_else(|e| panic!("{name}: {e:?}"));
                assert_eq!(write::build_lock(tid, file, &parsed), *req, "{name}");
                let reply = write::parse_lock_reply(rep.unwrap(), parsed.want_mod_count)
                    .unwrap_or_else(|e| panic!("{name} reply: {e:?}"));
                assert_eq!(reply.to_result().unwrap(), rep.unwrap(), "{name}");
                if let LockReply::Refused { status, .. } = reply {
                    assert_eq!(status, write::LOCK_CONFLICT, "{name}");
                }
            }
            op if op == Opcode::Unlock as u8 => {
                let parsed = write::parse_unlock_request(req).unwrap();
                assert_eq!(write::build_unlock(tid, file, &parsed), *req, "{name}");
                assert_eq!(write::echo_header(&header), rep.unwrap(), "{name}");
            }
            op if op == Opcode::KillAllLocks as u8 => {
                let parsed = write::parse_kill_all_locks_request(req).unwrap();
                assert_eq!(
                    write::build_kill_all_locks(tid, file, &parsed),
                    *req,
                    "{name}"
                );
                assert_eq!(write::echo_header(&header), rep.unwrap(), "{name}");
            }
            op if op == Opcode::RequestNotification as u8 => {
                let parsed = parse_request_notification(req).unwrap();
                assert_eq!(parsed.file_id, file, "{name}");
                assert_eq!(rep.unwrap(), [0, 0], "{name}");
            }
            other => panic!("{name}: unexpected opcode {other}"),
        }
        checked += 1;
    }
    assert!(checked >= 42, "{checked} vectors checked");
}
