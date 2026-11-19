//! Verify the Download (op90) reply path against the real captured session:
//! the first op90 reply after Open must yield the file's `mInternalCreation`.
//!
//! Ignored unless the s03 capture is present (it is git-ignored). Run with:
//!   cargo test -p om-protocol --test capture_download -- --ignored

use om_protocol::{parse_download_reply, parse_perform_reply};

const S2C: &str = "../../re/captures/2026-10-27-s03-direct/conn-000/s2c.bin";

/// Every logical GIOP message in `stream`, with GIOP 1.2 fragments joined.
fn logical_messages(stream: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut pending: Option<Vec<u8>> = None;
    let mut pos = 0;
    while pos + 12 <= stream.len() && &stream[pos..pos + 4] == b"GIOP" {
        let size = u32::from_le_bytes(stream[pos + 8..pos + 12].try_into().unwrap()) as usize;
        let end = pos + 12 + size;
        if end > stream.len() {
            break;
        }
        let msg = &stream[pos..end];
        let more = msg[6] & 2 != 0;
        match (msg[7], pending.as_mut()) {
            // A Fragment continues the pending message after its 4-byte request id.
            (7, Some(buf)) => {
                buf.extend_from_slice(&msg[16..]);
                if !more {
                    let mut whole = pending.take().unwrap();
                    let body_len = (whole.len() - 12) as u32;
                    whole[8..12].copy_from_slice(&body_len.to_le_bytes());
                    out.push(whole);
                }
            }
            _ if more => pending = Some(msg.to_vec()),
            _ => out.push(msg.to_vec()),
        }
        pos = end;
    }
    out
}

#[test]
#[ignore = "requires the git-ignored s03 capture"]
fn first_download_reply_yields_internal_creation() {
    let s2c = std::fs::read(S2C).expect("capture present");
    // The first op90 after Open is GIOP request id 8 (see tools/capture/giop_ops.py).
    let reply = logical_messages(&s2c)
        .into_iter()
        .find(|m| m[7] == 1 && u32::from_le_bytes(m[12..16].try_into().unwrap()) == 8)
        .expect("op90 reply present");

    let perform = parse_perform_reply(&reply).expect("split perform reply");
    let download = parse_download_reply(&perform.result).expect("parse download reply");
    assert_eq!(download.lock_error, 0);
    assert_eq!(
        *download.slots.last().unwrap(),
        0x40,
        "slot stream ends with 0x40"
    );

    let tree = om_hbam::status_tree_from_wire(&download.slots).expect("decode [2] subtree");
    let internal_creation = tree
        .internal_creation()
        .expect("read [2, 254]")
        .expect("[2, 254] present");
    // The hosted solution's value, confirmed against the shipped engine
    // (spec 4.3).
    assert_eq!(internal_creation, 0xc08c_3393);
}
