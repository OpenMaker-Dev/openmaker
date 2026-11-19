//! Verify the request-side protocol layer against a real captured session.
//!
//! Ignored unless the s03 capture is present (it is git-ignored and contains a
//! license key, so it exists only on the analysis host). Run with:
//!   cargo test -p om-protocol --test capture_c2s -- --ignored
use std::collections::BTreeMap;
use std::path::Path;

use om_giop::Request;
use om_protocol::{op, perform_transaction, request_body_offset};

fn messages(stream: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos + 12 <= stream.len() && &stream[pos..pos + 4] == b"GIOP" {
        let size = u32::from_le_bytes(stream[pos + 8..pos + 12].try_into().unwrap()) as usize;
        let end = pos + 12 + size;
        if end > stream.len() {
            break;
        }
        out.push(&stream[pos..end]);
        pos = end;
    }
    out
}

#[test]
#[ignore = "requires the git-ignored s03 capture"]
fn c2s_opcode_histogram_matches() {
    let path = Path::new("../../re/captures/2026-10-27-s03-direct/conn-000/c2s.bin");
    let stream = std::fs::read(path).expect("capture present");
    let mut ops: BTreeMap<u8, usize> = BTreeMap::new();
    let mut is_a = 0;
    let mut connect = 0;
    for msg in messages(&stream) {
        if msg[7] != 0 {
            continue; // requests only
        }
        let req = Request::parse(msg).expect("parse request");
        match req.operation {
            x if x == op::IS_A => is_a += 1,
            x if x == op::CONNECT => connect += 1,
            x if x == op::PERFORM => {
                let off = request_body_offset(msg.len(), req.body.len());
                let txn = perform_transaction(req.body, off).expect("perform txn");
                *ops.entry(txn[0]).or_default() += 1;
            }
            _ => {}
        }
    }
    // Totals observed with tools/capture/giop_ops.py.
    assert_eq!(is_a, 1);
    assert_eq!(connect, 1);
    assert_eq!(ops.get(&90), Some(&359)); // DownloadWithLock
    assert_eq!(ops.get(&86), Some(&96)); // QueryProgram
    assert_eq!(ops.get(&80), Some(&19)); // Login
    assert_eq!(ops.get(&78), Some(&19)); // OpenFile
    assert_eq!(ops.get(&71), Some(&4)); // HostCreateRecord
    assert_eq!(ops.get(&70), Some(&12)); // HostCommit
}

#[test]
#[ignore = "requires the git-ignored s03 capture"]
fn parses_connect_reply() {
    use om_protocol::parse_connect_reply;
    let s2c = std::fs::read("../../re/captures/2026-10-27-s03-direct/conn-000/s2c.bin")
        .expect("capture present");
    // The Connect reply is the GIOP Reply with request_id 4.
    let reply_msg = messages(&s2c)
        .into_iter()
        .find(|m| m[7] == 1 && u32::from_le_bytes(m[12..16].try_into().unwrap()) == 4)
        .expect("connect reply present");
    let reply = parse_connect_reply(reply_msg).expect("parse connect reply");
    assert_eq!(reply.connection_id, 0x463b_59ad);
    // The lab server's six-character name; the next field still parses.
    assert_eq!(reply.remote_name.chars().count(), 6);
    assert_eq!(reply.version_string, "Server 26.0.3");
    assert_eq!(reply.protocol_version, 0x20);
    assert_eq!(reply.install_code.len(), 32); // 32-hex server id
    assert!(reply.web_service_settings.starts_with("port=443"));
    assert_eq!(reply.standby_address, "");
    // The connection key is non-zero (the PBKDF2 salt).
    assert!(reply.connection_key.iter().any(|&b| b != 0));
}

#[test]
#[ignore = "requires the git-ignored s03 capture"]
fn parses_perform_open_and_login_replies() {
    use om_protocol::{parse_login_reply, parse_open_reply, parse_perform_reply};
    let s2c = std::fs::read("../../re/captures/2026-10-27-s03-direct/conn-000/s2c.bin")
        .expect("capture present");
    let by_id = |want: u32| {
        messages(&s2c)
            .into_iter()
            .find(|m| m[7] == 1 && u32::from_le_bytes(m[12..16].try_into().unwrap()) == want)
            .map(|m| m.to_vec())
    };

    // Open reply is request_id 6, Login reply is request_id 14 (from giop_ops).
    let open_msg = by_id(6).expect("open reply");
    let open_perform = parse_perform_reply(&open_msg).expect("split open reply");
    assert_eq!(open_perform.result.len(), 40);
    assert_eq!(open_perform.backchannel.first(), Some(&0xFE));
    let open = parse_open_reply(&open_perform.result).expect("parse open");
    assert_eq!(open.status, 0);
    // The server message is the server's name, as in the Connect reply.
    let connect = by_id(4).expect("connect reply");
    let connect = om_protocol::parse_connect_reply(&connect).expect("parse connect reply");
    assert_eq!(open.server_message, connect.remote_name);
    // The per-file nonce is some nonzero value (used for login key derivation).
    assert_ne!(open.file_nonce, 0);

    let login_msg = by_id(14).expect("login reply");
    let login_perform = parse_perform_reply(&login_msg).expect("split login reply");
    let login = parse_login_reply(&login_perform.result).expect("parse login");
    assert_eq!(login.error_code, 0); // the captured login succeeded
}

/// Rebuild both captured Logins with [`om_protocol::build_login`] and compare
/// byte-for-byte. Inputs come from the capture itself: the Connect key, each
/// Open reply, and each file's `mInternalCreation` (`[2, 254]`, read from the
/// first Download reply). The credentials are recovered in-process by
/// de-obscuring the captured fields and are never printed. Equality pins the
/// whole body layout and the obscuring chain.
#[test]
#[ignore = "requires the git-ignored s03 capture"]
fn rebuilds_captured_logins_byte_exact() {
    use om_protocol::{
        build_login, parse_connect_reply, parse_open_reply, parse_perform_reply, LoginEnvironment,
        LoginRequest,
    };
    let dir = "../../re/captures/2026-10-27-s03-direct/conn-000";
    let s2c = std::fs::read(format!("{dir}/s2c.bin")).expect("capture present");
    let c2s = std::fs::read(format!("{dir}/c2s.bin")).expect("capture present");
    let reply = |want: u32| {
        messages(&s2c)
            .into_iter()
            .find(|m| m[7] == 1 && u32::from_le_bytes(m[12..16].try_into().unwrap()) == want)
            .expect("reply present")
            .to_vec()
    };
    let request_txn = |want: u32| {
        let msg = messages(&c2s)
            .into_iter()
            .find(|m| m[7] == 0 && u32::from_le_bytes(m[12..16].try_into().unwrap()) == want)
            .expect("request present");
        let req = Request::parse(msg).unwrap();
        let off = request_body_offset(msg.len(), req.body.len());
        perform_transaction(req.body, off).unwrap().to_vec()
    };
    let conn_key = parse_connect_reply(&reply(4)).unwrap().connection_key;

    // (Open reply id, Login request id, file mInternalCreation)
    for (open_id, login_id, internal_creation) in [(6, 14, 0xc08c_3393), (50, 56, 0xc08c_3386)] {
        let open = parse_open_reply(&parse_perform_reply(&reply(open_id)).unwrap().result).unwrap();
        let captured = request_txn(login_id);
        let header = om_rctransaction::Header::parse(&captured).unwrap();

        // Recover the plaintext fields from the capture (tail-first pops).
        let hash = om_crypto::password_hash_for_login(
            om_crypto::login_nonce(open.file_nonce, internal_creation),
            &conn_key,
        );
        let mut r = om_rctransaction::Reader::parse(&captured).unwrap();
        assert_eq!(r.pop_u32().unwrap(), open.file_id);
        assert_eq!(r.pop_u8().unwrap(), 0);
        let session_name = r.pop_string().unwrap();
        let file_path = r.pop_string().unwrap();
        let app_language = r.pop_string().unwrap();
        let printer = r.pop_string().unwrap();
        let system_version = r.pop_string().unwrap();
        let body = &captured[..om_rctransaction::HEADER_LEN + r.remaining()];
        let deobscured = |end: usize| {
            let len = u32::from_be_bytes(body[end - 4..end].try_into().unwrap()) as usize;
            let mut field = body[end - 4 - len..end - 4].to_vec();
            om_crypto::deobscure_field(&mut field, &hash, &conn_key);
            (
                om_scsu::decode_scsu(&field).expect("valid SCSU"),
                end - 4 - len,
            )
        };
        let (account, end) = deobscured(body.len());
        let (password, end) = deobscured(end);
        // Below the password: an empty string (u32 len 0), the empty privilege
        // extension (u16 len 0), version 8 (u16); then the five paths after
        // the 7-byte prefix at body offset 20.
        let paths_end = end - 4 - 2 - 2;
        assert_eq!(&body[paths_end..end], &[0, 8, 0, 0, 0, 0, 0, 0]);
        let mut paths = om_rctransaction::ResultReader::new(&body[27..paths_end]);
        let application_path = paths.pop_string().unwrap();
        let desktop_path = paths.pop_string().unwrap();
        let documents_path = paths.pop_string().unwrap();
        let preferences_path = paths.pop_string().unwrap();
        let system_drive = paths.pop_string().unwrap();
        assert_eq!(paths.remaining(), 0);

        let environment = LoginEnvironment {
            system_drive,
            preferences_path,
            documents_path,
            desktop_path,
            application_path,
            system_version,
            printer,
            app_language,
            file_path,
            session_name,
        };
        let rebuilt = build_login(&LoginRequest {
            transaction_id: header.transid_lo,
            file_id: open.file_id,
            open_nonce: open.file_nonce,
            internal_creation,
            connection_key: conn_key,
            account: &account,
            password: &password,
            environment: &environment,
        });
        // The official client sends a random callback file-user id where
        // OpenMaker sends 0 (spec 5.4); everything else must match exactly.
        let mut expected = captured.clone();
        expected[20..24].fill(0);
        assert_eq!(rebuilt.len(), expected.len(), "login {login_id} length");
        assert!(
            rebuilt == expected,
            "login {login_id} differs from the capture"
        );
    }
}
