//! Verify the server side of the protocol layer against a real captured session:
//! every request the official client sent parses with the server parsers, and
//! the server's captured replies rebuild byte-for-byte from their parsed values.
//!
//! Ignored unless the s03 capture is present (it is git-ignored and holds a
//! license key and per-session secrets, so assertions never print bytes). Run:
//!   cargo test -p om-protocol --test capture_server -- --ignored

use om_giop::{Reply, Request};
use om_protocol::{
    build_connect_reply, build_download, build_is_a_reply, build_login, build_perform_reply, op,
    parse_connect_reply, parse_connect_request, parse_download_reply, parse_download_request,
    parse_is_a_request, parse_login_reply, parse_login_request, parse_open_reply,
    parse_open_request, parse_perform_reply, parse_perform_request, DownloadReply, DownloadRequest,
    LoginRequest, Opcode, REPO_ID,
};

const DIR: &str = "../../re/captures/2026-10-27-s03-direct/conn-000";

/// Every logical GIOP message in `stream`, with GIOP 1.2 fragments joined and
/// a flag saying whether the original was fragmented.
fn logical_messages(stream: &[u8]) -> Vec<(Vec<u8>, bool)> {
    let mut raw = om_giop::MessageReader::new(usize::MAX);
    let mut logical = om_giop::MessageReader::new(usize::MAX);
    let (mut raw_src, mut src) = (stream, stream);
    let mut out = Vec::new();
    loop {
        let first = match raw.read_message(&mut raw_src) {
            Ok(msg) => msg,
            Err(om_giop::ReadError::Closed) => return out,
            Err(e) => panic!("capture stream: {e}"),
        };
        let fragmented = first[6] & 2 != 0;
        // Keep the raw walk in step: skip this message's continuation fragments.
        let mut more = fragmented;
        while more {
            more = raw.read_message(&mut raw_src).unwrap()[6] & 2 != 0;
        }
        out.push((logical.read_logical(&mut src).unwrap(), fragmented));
    }
}

/// Whether a rebuilt `Perform` reply equals a captured one, ignoring the CDR
/// alignment padding before the backchannel length: FileMaker Server leaves
/// those bytes uninitialized, where OpenMaker writes zeros.
fn same_perform_reply(rebuilt: &[u8], captured: &[u8]) -> bool {
    if rebuilt.len() != captured.len() {
        return false;
    }
    // Reply header without service contexts: the body starts at offset 24.
    let result_len = u32::from_le_bytes(captured[24..28].try_into().unwrap()) as usize;
    let pad_start = 28 + result_len;
    let pad_end = pad_start.next_multiple_of(4);
    rebuilt[..pad_start] == captured[..pad_start] && rebuilt[pad_end..] == captured[pad_end..]
}

fn request_id(msg: &[u8]) -> u32 {
    u32::from_le_bytes(msg[12..16].try_into().unwrap())
}

struct Capture {
    requests: Vec<Vec<u8>>,
    replies: Vec<(Vec<u8>, bool)>,
}

impl Capture {
    fn load() -> Self {
        let c2s = std::fs::read(format!("{DIR}/c2s.bin")).expect("capture present");
        let s2c = std::fs::read(format!("{DIR}/s2c.bin")).expect("capture present");
        Self {
            requests: logical_messages(&c2s)
                .into_iter()
                .filter(|(m, _)| m[7] == 0)
                .map(|(m, _)| m)
                .collect(),
            replies: logical_messages(&s2c)
                .into_iter()
                .filter(|(m, _)| m[7] == 1)
                .collect(),
        }
    }

    fn reply(&self, id: u32) -> &(Vec<u8>, bool) {
        self.replies
            .iter()
            .find(|(m, _)| request_id(m) == id)
            .expect("reply present")
    }

    /// `(request id, transaction)` for every Perform with `opcode`.
    fn performs(&self, opcode: Opcode) -> Vec<(u32, Vec<u8>)> {
        self.requests
            .iter()
            .filter_map(|msg| {
                let req = Request::parse(msg).unwrap();
                (req.operation == op::PERFORM).then(|| {
                    let txn = parse_perform_request(req.body).unwrap().transaction;
                    (req.request_id, txn.to_vec())
                })
            })
            .filter(|(_, txn)| txn[0] == opcode as u8)
            .collect()
    }
}

#[test]
#[ignore = "requires the git-ignored s03 capture"]
fn server_parses_handshake_and_rebuilds_replies() {
    let cap = Capture::load();

    let is_a = Request::parse(&cap.requests[0]).unwrap();
    assert_eq!(is_a.operation, op::IS_A);
    assert_eq!(parse_is_a_request(is_a.body), Some(REPO_ID));
    assert!(build_is_a_reply(is_a.request_id, true) == cap.reply(is_a.request_id).0);

    let connect = cap
        .requests
        .iter()
        .find(|m| Request::parse(m).unwrap().operation == op::CONNECT)
        .unwrap();
    let request = parse_connect_request(connect).expect("parse official Connect");
    assert_eq!(request.callback_type_id, "IDL:RCFMP12:1.0");
    assert_eq!(request.machine.version_string, "Pro 26.0.3");
    assert_eq!(request.machine.protocol_version, 0x20);

    let reply = &cap.reply(request_id(connect)).0;
    let identity = parse_connect_reply(reply).unwrap();
    assert!(
        build_connect_reply(request_id(connect), &identity) == *reply,
        "Connect reply rebuilds byte-for-byte"
    );
}

#[test]
#[ignore = "requires the git-ignored s03 capture"]
fn server_parses_opens_and_rebuilds_replies() {
    let cap = Capture::load();
    let opens = cap.performs(Opcode::OpenFile);
    assert_eq!(opens.len(), 19);
    for (id, txn) in opens {
        let open = parse_open_request(&txn).expect("parse official Open");
        assert!(open.filename.ends_with(".fmp12"));
        assert_eq!(open.version, 8);

        let (msg, fragmented) = cap.reply(id);
        assert!(!fragmented);
        let perform = parse_perform_reply(msg).unwrap();
        let reply = parse_open_reply(&perform.result).expect("successful Open");
        assert!(
            reply.to_result() == perform.result,
            "Open {id} result rebuilds"
        );
        assert!(
            same_perform_reply(
                &build_perform_reply(id, &perform.result, &perform.backchannel),
                msg
            ),
            "Open {id} reply message rebuilds"
        );
    }
}

#[test]
#[ignore = "requires the git-ignored s03 capture"]
fn server_parses_downloads_and_rebuilds_replies() {
    use om_protocol::{NodeState, Resume};
    let cap = Capture::load();
    let downloads = cap.performs(Opcode::DownloadWithLock);
    assert_eq!(downloads.len(), 359);
    let (mut nodes, mut terminal, mut missing) = (0, Vec::new(), 0);
    for (id, txn) in downloads {
        let request = parse_download_request(&txn).expect("parse official Download");
        assert_eq!(request.resume, Resume::start(), "no paging in s03");
        let segments: Vec<&[u8]> = request.key_path.iter().map(Vec::as_slice).collect();
        let header = om_rctransaction::Header::parse(&txn).unwrap();
        let rebuilt = build_download(&DownloadRequest {
            transaction_id: header.transid_lo,
            file_id: header.file_id,
            key_path: &segments,
            resume: Resume::start(),
        });
        // The official client reports its idle seconds in the header; the
        // builder always sends 0 (spec 4.1).
        let mut expected = txn.clone();
        expected[16..20].fill(0);
        assert!(
            rebuilt == expected,
            "Download {id} request rebuilds from its key path"
        );

        let (msg, _) = cap.reply(id);
        let perform = parse_perform_reply(msg).unwrap();
        let reply = parse_download_reply(&perform.result).expect("parse Download reply");
        assert!(
            reply.to_result() == perform.result,
            "Download {id} result rebuilds"
        );
        assert_eq!(reply.lock_error, 0);

        // Each reply is one of the three shapes a server builds (spec 5.6.6c),
        // with the slots re-encoded from the decoded subtree, which is how
        // om-server produces them from a file.
        let rebuilt = match reply.node_state {
            NodeState::NonTerminal => {
                nodes += 1;
                let base = request
                    .key_path
                    .iter()
                    .map(|s| om_hbam::KeySegment(s))
                    .collect();
                let tree = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::with_path(
                    &reply.slots,
                    base,
                ))
                .expect("decode downloaded subtree");
                // An existing but empty list decodes to no node; it is `80 40`.
                let wire = match tree.node(&segments) {
                    Some(node) => node.to_wire().unwrap(),
                    None => vec![0x80, 0x40],
                };
                DownloadReply::node(wire, Resume::complete())
            }
            NodeState::Terminal => {
                terminal.push(id);
                DownloadReply::terminal(reply.value.clone())
            }
            NodeState::Missing => {
                missing += 1;
                DownloadReply::missing()
            }
            NodeState::Other(state) => panic!("Download {id}: node state {state}"),
        };
        assert!(
            rebuilt.to_result() == perform.result,
            "Download {id} rebuilds from the file's view of the key"
        );
    }
    assert_eq!(nodes, 356);
    assert_eq!(missing, 2);
    // The one key holding a value: [0x1f, 5, 0x58, "SVG "], an icon.
    assert_eq!(terminal, [568]);
}

#[test]
#[ignore = "requires the git-ignored s03 capture"]
fn server_recovers_captured_logins_and_rebuilds_replies() {
    let cap = Capture::load();
    let connect = cap
        .requests
        .iter()
        .find(|m| Request::parse(m).unwrap().operation == op::CONNECT)
        .unwrap();
    let conn_key = parse_connect_reply(&cap.reply(request_id(connect)).0)
        .unwrap()
        .connection_key;
    let logins = cap.performs(Opcode::Login);

    // (Open reply id, Login request id, file mInternalCreation): the two files
    // whose mInternalCreation is known (spec 4.3).
    for (open_id, login_id, internal_creation) in [(6, 14, 0xc08c_3393), (50, 56, 0xc08c_3386)] {
        let open =
            parse_open_reply(&parse_perform_reply(&cap.reply(open_id).0).unwrap().result).unwrap();
        let (_, txn) = logins.iter().find(|(id, _)| *id == login_id).unwrap();
        let login = parse_login_request(txn, open.file_nonce, internal_creation, &conn_key)
            .expect("parse official Login");
        assert_eq!(login.file_id, open.file_id);
        assert_eq!(login.login_type, 0);
        assert!(!login.account.is_empty());
        assert!(!login.has_new_password);

        // Parse -> build reproduces the capture, except the random callback
        // file-user id OpenMaker always sends as 0 (spec 5.4).
        let header = om_rctransaction::Header::parse(txn).unwrap();
        let rebuilt = build_login(&LoginRequest {
            transaction_id: header.transid_lo,
            file_id: login.file_id,
            open_nonce: open.file_nonce,
            internal_creation,
            connection_key: conn_key,
            account: &login.account,
            password: &login.password,
            environment: &login.environment,
        });
        let mut expected = txn.clone();
        expected[20..24].fill(0);
        assert!(rebuilt == expected, "Login {login_id} round-trips");

        let perform = parse_perform_reply(&cap.reply(login_id).0).unwrap();
        let reply = parse_login_reply(&perform.result).unwrap();
        assert!(reply.is_success());
        assert!(
            reply.to_result() == perform.result,
            "Login {login_id} result rebuilds"
        );
    }
}

#[test]
#[ignore = "requires the git-ignored s03 capture"]
fn every_reply_splits_into_two_sequences() {
    let cap = Capture::load();
    for (msg, _) in &cap.replies {
        let reply = Reply::parse(msg).unwrap();
        if reply.request_id <= 4 {
            continue; // _is_a and Connect are not Performs
        }
        let perform = parse_perform_reply(msg).expect("perform reply splits");
        let rebuilt = build_perform_reply(reply.request_id, &perform.result, &perform.backchannel);
        assert!(
            same_perform_reply(&rebuilt, msg),
            "reply {} rebuilds",
            reply.request_id
        );
    }
}

#[test]
#[ignore = "requires the git-ignored s03 capture"]
fn server_parses_download_lists_and_rebuilds_replies() {
    use om_protocol::{
        build_download_list, key_list_keys, parse_download_list_reply, parse_download_list_request,
        DownloadListRequest, Resume,
    };
    let cap = Capture::load();
    let lists = cap.performs(Opcode::DownloadList);
    assert_eq!(lists.len(), 13);
    for (id, txn) in lists {
        let list = parse_download_list_request(&txn).expect("parse official DownloadList");
        assert_eq!(list.resume, Resume::start(), "no paging in s03");
        let path: Vec<&[u8]> = list.key_path.iter().map(Vec::as_slice).collect();
        let list_keys = key_list_keys(&list.payload).expect("complete key list");
        let keys: Vec<&[u8]> = list_keys.iter().map(Vec::as_slice).collect();
        let header = om_rctransaction::Header::parse(&txn).unwrap();
        let rebuilt = build_download_list(&DownloadListRequest {
            transaction_id: header.transid_lo,
            file_id: header.file_id,
            key_path: &path,
            keys: &keys,
            flag: list.flag,
            resume: Resume::start(),
        });
        let mut expected = txn.clone();
        expected[16..20].fill(0); // idle seconds (spec 4.1)
        assert!(rebuilt == expected, "DownloadList {id} request rebuilds");

        let perform = parse_perform_reply(&cap.reply(id).0).unwrap();
        let reply = parse_download_list_reply(&perform.result).expect("DownloadList reply");
        assert_eq!(reply.status, 0);
        assert!(
            reply.to_result() == perform.result,
            "DownloadList {id} result rebuilds"
        );

        // The reply is the parent restricted to requested children that exist,
        // in the Download wire form.
        let base = list
            .key_path
            .iter()
            .map(|s| om_hbam::KeySegment(s))
            .collect();
        let tree = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::with_path(&reply.slots, base))
            .unwrap();
        let parent = tree.node(&path).expect("parent present");
        assert!(parent
            .children
            .iter()
            .all(|child| list_keys.iter().any(|k| k.as_slice() == child.segment.0)));
        assert!(parent.to_wire().as_deref() == Some(&reply.slots[..]));
    }
}

#[test]
#[ignore = "requires the git-ignored s03 capture"]
fn server_parses_request_notifications() {
    use om_protocol::parse_request_notification;
    let cap = Capture::load();
    let requests = cap.performs(Opcode::RequestNotification);
    assert_eq!(requests.len(), 19);
    for (id, txn) in requests {
        let header = om_rctransaction::Header::parse(&txn).unwrap();
        let request = parse_request_notification(&txn).expect("parse official op63");
        assert_eq!(request.file_id, header.file_id);
        assert_eq!(
            (request.attention_mask, request.message_address),
            (u32::MAX, 1)
        );
        assert!(request.path.is_empty());
        assert!(request.client_file_id < 0);
        let perform = parse_perform_reply(&cap.reply(id).0).unwrap();
        assert_eq!(perform.result, [0, 0], "op63 {id} reply is status 0");
    }
}

#[test]
#[ignore = "requires the git-ignored s03 capture"]
fn unlock_replies_echo_the_request_header() {
    let cap = Capture::load();
    let unlocks = cap.performs(Opcode::Unlock);
    assert_eq!(unlocks.len(), 19);
    for (id, txn) in unlocks {
        let mut header = om_rctransaction::Header::parse(&txn).unwrap();
        header.idle_time = 0;
        let mut expected = Vec::new();
        header.write(&mut expected);
        let perform = parse_perform_reply(&cap.reply(id).0).unwrap();
        assert!(
            perform.result == expected,
            "op62 {id} reply echoes its header"
        );
    }
}

#[test]
#[ignore = "requires the git-ignored s03 capture"]
fn server_serves_the_security_catalog_entry_before_login() {
    // The first file's exchanges: Open (6), Download [2] (8), op63 (10),
    // Download [23, 1, 1] (12), then Login (14).
    let cap = Capture::load();
    let downloads = cap.performs(Opcode::DownloadWithLock);
    let (id, txn) = downloads.iter().find(|(id, _)| *id == 12).unwrap();
    assert_eq!(
        parse_download_request(txn).unwrap().key_path,
        vec![vec![23], vec![1], vec![1]]
    );
    let logins = cap.performs(Opcode::Login);
    assert!(
        logins.iter().all(|(login, _)| login > id),
        "requested before Login"
    );
    let reply =
        parse_download_reply(&parse_perform_reply(&cap.reply(*id).0).unwrap().result).unwrap();
    assert_eq!(reply.lock_error, 0);
    assert!(!reply.slots.is_empty(), "served with data before Login");
}

#[test]
#[ignore = "requires the git-ignored s03 capture"]
fn query_programs_parse_rebuild_and_carry_valid_access_keys() {
    use om_protocol::{build_query, parse_query_reply, parse_query_request, QueryRequest, Resume};
    let cap = Capture::load();
    let connect = cap
        .requests
        .iter()
        .find(|m| Request::parse(m).unwrap().operation == op::CONNECT)
        .unwrap();
    let conn_key = parse_connect_reply(&cap.reply(request_id(connect)).0)
        .unwrap()
        .connection_key;
    let queries = cap.performs(Opcode::QueryProgram);
    assert_eq!(queries.len(), 96);
    let mut instructions = 0;
    for (id, txn) in queries {
        let query = parse_query_request(&txn).expect("parse official op86");
        assert_eq!(query.resume, Resume::start());
        let header = om_rctransaction::Header::parse(&txn).unwrap();
        let mut expected = txn.clone();
        expected[16..20].fill(0); // idle seconds (spec 4.1)
        let rebuilt = build_query(&QueryRequest {
            transaction_id: header.transid_lo,
            file_id: header.file_id,
            program: &query.program,
            resume: Resume::start(),
        });
        assert!(rebuilt == expected, "op86 {id} request rebuilds");

        // Every instruction's [1, i, 7] is MD5(i, connection key, file id).
        let tree = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::new(&query.program)).unwrap();
        for instruction in &tree.node(&[&[1]]).expect("[1] instructions").children {
            let i = om_hbam::serial::decode(instruction.segment.0).unwrap() as u32;
            let file_id = u32::from_be_bytes(
                instruction
                    .child(&[4])
                    .unwrap()
                    .value
                    .unwrap()
                    .try_into()
                    .unwrap(),
            );
            assert_eq!(file_id, header.file_id);
            let key = instruction.child(&[7]).unwrap().value.unwrap();
            assert_eq!(
                key,
                om_crypto::query_access_key(i, &conn_key, file_id),
                "op86 {id} instruction {i}"
            );
            instructions += 1;
        }

        let perform = parse_perform_reply(&cap.reply(id).0).unwrap();
        let reply = parse_query_reply(&perform.result).expect("op86 reply");
        assert_eq!(reply.resume, Resume::complete());
        assert!(
            reply.to_result() == perform.result,
            "op86 {id} result rebuilds"
        );
    }
    assert_eq!(instructions, 98);
}
