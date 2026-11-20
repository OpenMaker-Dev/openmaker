//! The desktop client reads container data from the hosted file's library
//! (containers.md 6.1) against an `om-server` on loopback, over the
//! in-memory Tasks file of `om-server/tests/fixtures/tasks.rs` with a
//! container field Photo whose record 1 names library member 1 (a 3 x 2
//! PNG named `photo2.png`).
//!
//! A calculation reading the field gets the member's file name (its
//! header); the data is fetched only when a function reads it.

#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
#[path = "../../om-server/tests/fixtures/relay.rs"]
mod relay;

use std::cell::RefCell;
use std::path::Path;

use fixture::{TempFile, ACCOUNT, FILE, OCCURRENCE, PASSWORD, PHOTO_MEMBER, TABLE};
use om_data::calc::eval::{self, Context, Variables};
use om_data::calc::Container;
use om_data::host::{CachedSource, CalcData, CalcNeed, FileCalc, FileHost, SessionInfo};
use om_data::{Connection, DataProvider, FieldChanges};
use om_hbam::serial;
use om_hbam::slot::uint_segment;
use om_protocol::{LoginEnvironment, MachineInfo};
use om_provider::{NativeConfig, NativeProvider};
use om_server::{HostedFile, Identity, Server, ServerConfig, ServerIdentity};

fn host(path: &Path) -> (u16, Vec<u8>) {
    host_recording(path, None)
}

/// [`host`], recording every connection under `record_dir` when given
/// (`om-server --record`), for [`library_reads`].
fn host_recording(path: &Path, record_dir: Option<&Path>) -> (u16, Vec<u8>) {
    let generated = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let ca_pem = generated.cert.pem().into_bytes();
    let identity =
        Identity::from_pem(&ca_pem, generated.key_pair.serialize_pem().as_bytes()).unwrap();
    let config = ServerConfig {
        identity: ServerIdentity {
            name: "openmaker-test".into(),
            version_string: "OpenMaker Server test".into(),
            server_id: "0123456789abcdef0123456789abcdef".into(),
            web_service_settings: String::new(),
        },
        files: vec![HostedFile {
            name: FILE.into(),
            path: path.to_path_buf(),
        }],
        insecure_files: false,
        record_dir: record_dir.map(Path::to_path_buf),
    };
    let server = Server::bind("127.0.0.1:0", config, identity).unwrap();
    let port = server.local_addr().unwrap().port();
    std::thread::spawn(move || server.serve());
    (port, ca_pem)
}

fn open(port: u16, ca_pem: &[u8]) -> NativeProvider {
    let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
    let mut provider = NativeProvider::new(NativeConfig {
        client_cert_pem: client.cert.pem().into_bytes(),
        client_key_pem: client.key_pair.serialize_pem().into_bytes(),
        additional_ca_pem: ca_pem.to_vec(),
        callback_host: "127.0.0.1".into(),
        machine: MachineInfo::default(),
    });
    provider
        .open(&Connection {
            host: "127.0.0.1".into(),
            port,
            file: FILE.into(),
            account: ACCOUNT.into(),
            password: PASSWORD.into(),
        })
        .unwrap();
    provider
}

fn file(tag: &str) -> TempFile {
    let mut root = fixture::tree(&[], true);
    fixture::add_photo(&mut root);
    TempFile::from_tree(tag, &root)
}

/// Evaluate `text` on record 1 of Tasks through a cache that fetches from
/// the provider, as the desktop's script host does; returns the result and
/// every need the evaluation fetched.
fn evaluate(
    provider: &mut NativeProvider,
    cache: &RefCell<CalcData>,
    text: &str,
) -> (String, Vec<CalcNeed>) {
    let schema = provider.calc_schema().unwrap().unwrap();
    let provider = RefCell::new(provider);
    let fetched = RefCell::new(Vec::new());
    let fetch = |needs: &[CalcNeed]| {
        fetched.borrow_mut().extend(needs.iter().cloned());
        provider.borrow_mut().calc_data(needs).ok()
    };
    let source = CachedSource::new(cache).with_fetch(&fetch);
    let today = om_data::calc::Date::from_ymd(2026, 10, 29).unwrap();
    let session = SessionInfo::default();
    let (mut locals, mut globals) = (Variables::new(), Variables::new());
    let mut host = FileHost::new(
        FileCalc::new(&schema, &source, today),
        &session,
        &mut locals,
        &mut globals,
    );
    let result = eval::evaluate_text(text, &mut host, &Context::record(OCCURRENCE, 1));
    let shown = eval::display(&result.value, &host);
    (shown, fetched.take())
}

#[test]
fn calculations_read_the_file_name_and_load_data_on_demand() {
    let file = file("containers-read");
    let (port, ca_pem) = host(&file.0);
    let mut provider = open(port, &ca_pem);
    let member = serial::encode(PHOTO_MEMBER);
    let header = CalcNeed::Member {
        table: TABLE,
        member: member.clone(),
    };
    let data = CalcNeed::MemberData {
        table: TABLE,
        member: member.clone(),
    };
    let cache = RefCell::new(CalcData::default());

    // GetAsText shows the stored file name: the header, not the data.
    let (text, fetched) = evaluate(&mut provider, &cache, "GetAsText ( Tasks::Photo )");
    assert_eq!(text, "photo2.png");
    assert!(fetched.contains(&header));
    assert!(!fetched.contains(&data));

    // The functions that read data load it once.
    let (text, fetched) = evaluate(
        &mut provider,
        &cache,
        "GetWidth ( Tasks::Photo ) & \"x\" & GetHeight ( Tasks::Photo ) & \"|\" & \
         Length ( Tasks::Photo ) & \"|\" & VerifyContainer ( Tasks::Photo ) & \"|\" & \
         GetContainerAttribute ( Tasks::Photo ; \"filename\" )",
    );
    assert_eq!(text, "3x2|73|?|photo2.png");
    assert_eq!(fetched, std::slice::from_ref(&data));
    let (_, fetched) = evaluate(&mut provider, &cache, "Base64Encode ( Tasks::Photo )");
    assert!(fetched.is_empty(), "{fetched:?}");

    // ExecuteSQL shows the file name too (execute-sql.md 8.1).
    let (text, _) = evaluate(
        &mut provider,
        &cache,
        "ExecuteSQL ( \"SELECT Photo FROM Tasks WHERE Name = 'Write tests'\" ; \"\" ; \"\" )",
    );
    assert_eq!(text, "photo2.png");

    // A provider's answer carries the member as asked.
    let answer = provider.calc_data(&[header, data]).unwrap();
    assert_eq!(answer.library.members.len(), 1);
    assert_eq!(answer.library.data.len(), 1);
    provider.close();
}

/// The stored bytes at `path`, as a leaf or reassembled from the node
/// FileMaker's text form makes of a container reference (containers.md
/// 1.4).
fn stored_bytes(session: &mut om_session::Session, path: &[Vec<u8>]) -> Vec<u8> {
    let refs: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
    let stored = session.download(&refs).unwrap();
    if stored.node_state == om_protocol::NodeState::Terminal {
        return stored.value;
    }
    let tree = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::with_path(
        &stored.slots,
        path.iter().map(|p| om_hbam::KeySegment(p)).collect(),
    ))
    .unwrap();
    tree.node(&refs).unwrap().assembled().unwrap().into_owned()
}

/// A session of its own, to read the library's counts.
fn raw_session(port: u16) -> om_session::Session {
    let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
    let mut session = om_session::Session::connect(
        "127.0.0.1",
        port,
        client.cert.pem().as_bytes(),
        client.key_pair.serialize_pem().as_bytes(),
        "127.0.0.1",
        &MachineInfo::default(),
    )
    .unwrap();
    session.open_file("127.0.0.1", FILE).unwrap();
    let creation = session.fetch_internal_creation().unwrap();
    let environment = LoginEnvironment::for_remote_file("127.0.0.1", FILE);
    assert!(session
        .login(ACCOUNT, PASSWORD, creation, &environment)
        .unwrap()
        .is_success());
    session
}

/// The reference count of member `member` of Tasks' library; `None` when
/// the library no longer has it.
fn count(session: &mut om_session::Session, member: u64) -> Option<u32> {
    let (t, m) = (uint_segment(TABLE), serial::encode(member));
    let reply = session.download(&[&t, &[0x1f], &[5], &m, &[0x12]]).unwrap();
    (reply.node_state == om_protocol::NodeState::Terminal)
        .then(|| u32::from_be_bytes(reply.value.as_slice().try_into().unwrap()))
}

fn photo(text: Option<&str>) -> FieldChanges {
    [("Photo".to_string(), text.map(str::to_string))]
        .into_iter()
        .collect()
}

/// The client stores data with op88 before it commits, takes a reference
/// for a copy of the table's own data with op87, and releases what a commit
/// replaces or clears and what a deleted record held (containers.md 5.1,
/// 7.1-7.3).
#[test]
fn writes_store_and_release_container_data_as_filemaker_pro_does() {
    let file = file("containers-write");
    let (port, ca_pem) = host(&file.0);
    let mut provider = open(port, &ca_pem);
    let mut counts = raw_session(port);
    let tasks = provider
        .layouts()
        .unwrap()
        .into_iter()
        .find(|l| l.name == "Tasks")
        .unwrap()
        .id;

    // New data: member 2, its one reference held for the value.
    let data = Container::file("a.txt", b"hello".to_vec());
    let reference = provider
        .container_reference(tasks, "Photo", &data)
        .unwrap()
        .unwrap();
    assert_eq!(reference, "02\u{e000}");
    assert_eq!(count(&mut counts, 2), Some(1));
    provider
        .update_record(tasks, 2, photo(Some(&reference)))
        .unwrap();
    assert_eq!(count(&mut counts, 2), Some(1));

    // Record 1's value, copied into record 2: one more reference to member
    // 1; the commit releases record 2's member 2, which goes.
    let first = {
        let answer = provider
            .calc_data(&[CalcNeed::Member {
                table: TABLE,
                member: serial::encode(PHOTO_MEMBER),
            }])
            .unwrap();
        let info = answer
            .library
            .members
            .values()
            .next()
            .cloned()
            .flatten()
            .unwrap();
        info.value(TABLE, &serial::encode(PHOTO_MEMBER))
    };
    let copied = provider
        .container_reference(tasks, "Photo", &first)
        .unwrap()
        .unwrap();
    assert_eq!(copied, "01\u{e000}");
    assert_eq!(count(&mut counts, 1), Some(2));
    provider
        .update_record(tasks, 2, photo(Some(&copied)))
        .unwrap();
    assert_eq!(count(&mut counts, 2), None);
    assert_eq!(count(&mut counts, 1), Some(2));

    // Text is kept as text, without a member.
    assert_eq!(
        provider
            .container_reference(
                tasks,
                "Photo",
                &Container {
                    text: om_data::calc::Text::plain("image:/a.png"),
                    ..Container::default()
                }
            )
            .unwrap(),
        None
    );

    // Deleting record 2 releases its reference; clearing record 1 the last.
    provider.delete_record(tasks, 2).unwrap();
    assert_eq!(count(&mut counts, 1), Some(1));
    provider.update_record(tasks, 1, photo(None)).unwrap();
    assert_eq!(count(&mut counts, 1), None);
    provider.close();
}

#[test]
fn rolled_back_and_superseded_transaction_uploads_release_only_their_own_references() {
    use om_data::runtime::TransactionOptions;
    for outcome in ["commit", "revert", "close"] {
        let commit = outcome == "commit";
        let file = file(if commit {
            "container-tx-commit"
        } else {
            "container-tx-revert"
        });
        let (port, ca_pem) = host(&file.0);
        let mut provider = open(port, &ca_pem);
        let tasks = tasks_layout(&mut provider);
        let mut counts = raw_session(port);
        provider
            .begin_transaction(TransactionOptions::default())
            .unwrap();
        for (name, bytes) in [
            ("first.txt", b"first".as_slice()),
            ("second.txt", b"second".as_slice()),
        ] {
            let reference = provider
                .container_reference(tasks, "Photo", &Container::file(name, bytes.to_vec()))
                .unwrap()
                .unwrap();
            provider
                .update_record(tasks, 2, photo(Some(&reference)))
                .unwrap();
        }
        assert_eq!(count(&mut counts, 2), Some(1));
        assert_eq!(count(&mut counts, 3), Some(1));
        match outcome {
            "commit" => provider.commit_transaction(false).unwrap(),
            "revert" => provider.revert_transaction().unwrap(),
            _ => provider.close(),
        }
        assert_eq!(count(&mut counts, 1), Some(1), "original stored data");
        assert_eq!(count(&mut counts, 2), None, "superseded upload");
        assert_eq!(count(&mut counts, 3), commit.then_some(1), "last upload");
        provider.close();
    }
}

#[test]
fn a_definitively_failed_transaction_releases_uploads_before_commit_submission() {
    use om_data::runtime::TransactionOptions;
    let file = file("container-tx-stale-refusal");
    let (port, ca_pem) = host(&file.0);
    let mut provider = open(port, &ca_pem);
    let tasks = tasks_layout(&mut provider);
    let mut counts = raw_session(port);
    provider
        .begin_transaction(TransactionOptions::default())
        .unwrap();
    let reference = provider
        .container_reference(
            tasks,
            "Photo",
            &Container::file("refused.txt", b"not stored".to_vec()),
        )
        .unwrap()
        .unwrap();
    let mut changes = photo(Some(&reference));
    changes.insert("Code".into(), Some("TOOLONG".into()));
    provider.update_record(tasks, 2, changes).unwrap();
    assert_eq!(
        provider.commit_transaction(false).unwrap_err().code,
        Some(507)
    );
    assert_eq!(count(&mut counts, 2), None);
    assert_eq!(count(&mut counts, 1), Some(1));
    provider.close();
}

#[test]
fn a_lost_commit_reply_keeps_references_that_the_server_already_stored() {
    use om_data::runtime::TransactionOptions;
    use std::time::Duration;
    for corrupt in [false, true] {
        let file = file(if corrupt {
            "container-corrupt-commit"
        } else {
            "container-ambiguous-commit"
        });
        let (port, _) = host(&file.0);
        let relay = relay::Relay::start(port, Duration::ZERO);
        let mut provider = open(relay.port, &relay.ca_pem);
        let tasks = tasks_layout(&mut provider);
        provider
            .begin_transaction(TransactionOptions::default())
            .unwrap();
        let reference = provider
            .container_reference(
                tasks,
                "Photo",
                &Container::file("committed.txt", b"stored despite the lost reply".to_vec()),
            )
            .unwrap()
            .unwrap();
        provider
            .update_record(tasks, 2, photo(Some(&reference)))
            .unwrap();
        if corrupt {
            relay.corrupt_reply_after(om_protocol::Opcode::HostCommit);
        } else {
            relay.lose_reply_after(om_protocol::Opcode::HostCommit);
        }
        let mark = provider.submission_mark().unwrap();
        assert!(provider.commit_transaction(false).is_err());
        // The host applied it: the save cannot be taken as unsent (#152).
        assert_eq!(
            mark.until(provider.submission_mark()),
            om_data::Submitted::Possibly
        );
        if corrupt {
            assert!(
                provider.connection_lost().is_none(),
                "the protocol error leaves a usable transport"
            );
        }
        let mut counts = raw_session(port);
        assert_eq!(
            count(&mut counts, 2),
            Some(1),
            "an ambiguous outcome must not delete a stored member"
        );
        let path = [
            uint_segment(TABLE),
            vec![5],
            serial::encode(2),
            uint_segment(fixture::PHOTO),
        ];
        let stored = stored_bytes(&mut counts, &path);
        assert_eq!(om_scsu::decode_scsu(&stored).unwrap(), reference);
        provider.close();
    }
}

/// A release the host applied whose reply is lost (unreadable, on a
/// connection that stays usable) leaves nothing for a later transaction to
/// revert: the window took one more reference to record 1's member, gave it
/// back, and the revert of a transaction opened after must not give it back
/// again, which would delete the member record 1 still uses.
#[test]
fn a_release_whose_reply_is_lost_is_not_released_again_by_a_later_revert() {
    use om_data::runtime::TransactionOptions;
    let file = file("container-lost-release");
    let (port, _) = host(&file.0);
    let relay = relay::Relay::start(port, std::time::Duration::ZERO);
    let mut provider = open(relay.port, &relay.ca_pem);
    let mut counts = raw_session(port);
    let tasks = tasks_layout(&mut provider);
    let reference = "01\u{e000}";
    provider.adjust_container(tasks, reference, 1).unwrap();
    assert_eq!(count(&mut counts, PHOTO_MEMBER), Some(2));
    relay.corrupt_reply_after(om_protocol::Opcode::AdjustReferenceCount);
    assert!(provider.adjust_container(tasks, reference, -1).is_err());
    assert!(
        provider.connection_lost().is_none(),
        "the protocol error leaves a usable transport"
    );
    assert_eq!(count(&mut counts, PHOTO_MEMBER), Some(1));
    provider
        .begin_transaction(TransactionOptions::default())
        .unwrap();
    provider.revert_transaction().unwrap();
    assert_eq!(
        count(&mut counts, PHOTO_MEMBER),
        Some(1),
        "record 1 still holds member 1"
    );
    provider.close();
}

/// Data larger than one request goes up in parts (containers.md 3.3) and
/// reads back whole.
#[test]
fn large_container_data_uploads_in_parts() {
    let file = file("containers-large");
    let (port, ca_pem) = host(&file.0);
    let mut provider = open(port, &ca_pem);
    let tasks = provider
        .layouts()
        .unwrap()
        .into_iter()
        .find(|l| l.name == "Tasks")
        .unwrap()
        .id;
    // 2.5 MB that do not compress: three parts.
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let data: Vec<u8> = (0..2_500_000)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    let reference = provider
        .container_reference(tasks, "Photo", &Container::file("big.bin", data.clone()))
        .unwrap()
        .unwrap();
    assert_eq!(reference, "02\u{e000}");
    provider
        .update_record(tasks, 2, photo(Some(&reference)))
        .unwrap();
    let member = serial::encode(2);
    let answer = provider
        .calc_data(&[CalcNeed::MemberData {
            table: TABLE,
            member: member.clone(),
        }])
        .unwrap();
    let stored = answer
        .library
        .data
        .get(&(TABLE, member))
        .cloned()
        .flatten()
        .unwrap();
    assert!(stored.intact);
    assert_eq!(stored.data.text.as_str(), "big.bin");
    assert_eq!(stored.data.data(), data.as_slice());
    provider.close();
}

/// The desktop's own writes compute stored and auto-enter calculations on
/// a container's data (script oracle s34-containers-calc), loading the
/// members the record names.
#[test]
fn calculations_of_a_write_read_container_data() {
    let mut root = fixture::tree(&[], true);
    fixture::add_photo(&mut root);
    fixture::add_photo_calculations(&mut root);
    let file = TempFile::from_tree("containers-calc", &root);
    let (port, ca_pem) = host(&file.0);
    let mut provider = open(port, &ca_pem);
    let tasks = provider
        .layouts()
        .unwrap()
        .into_iter()
        .find(|l| l.name == "Tasks")
        .unwrap()
        .id;
    let value = |page: &om_data::RecordPage, id: i64, field: &str| -> Option<String> {
        let column = page.fields.iter().position(|f| f.name == field)?;
        page.records.iter().find(|r| r.id == id)?.values[column].clone()
    };
    let reference = provider
        .container_reference(tasks, "Photo", &Container::file("a.txt", b"hello".to_vec()))
        .unwrap()
        .unwrap();
    provider
        .update_record(tasks, 2, photo(Some(&reference)))
        .unwrap();
    let page = provider.records(tasks, 0, 10).unwrap();
    assert_eq!(value(&page, 2, "PhotoName").as_deref(), Some("a.txt"));
    assert_eq!(value(&page, 2, "PhotoLength").as_deref(), Some("5"));
    assert_eq!(value(&page, 2, "PhotoB64").as_deref(), Some("aGVsbG8=\r\n"));
    assert_eq!(value(&page, 2, "PhotoAE").as_deref(), Some("5|a.txt"));
    // Text is text; a cleared value empties them.
    provider
        .update_record(tasks, 2, photo(Some("hello")))
        .unwrap();
    let page = provider.records(tasks, 0, 10).unwrap();
    assert_eq!(value(&page, 2, "PhotoName"), None);
    assert_eq!(value(&page, 2, "PhotoAE").as_deref(), Some("5|hello"));
    provider.update_record(tasks, 2, photo(None)).unwrap();
    let page = provider.records(tasks, 0, 10).unwrap();
    assert_eq!(value(&page, 2, "PhotoLength"), None);
    assert_eq!(value(&page, 2, "PhotoAE"), None);
    provider.close();
}

#[test]
fn cross_table_copies_keep_the_stored_path_type_and_all_streams() {
    let mut root = fixture::tree(&[], true);
    fixture::add_photo(&mut root);
    fixture::add_field(&mut root, fixture::NOTES, 20, "Photo", 1, 6, 0, 0, 0);
    let file = TempFile::from_tree("containers-cross-table", &root);
    let (port, ca_pem) = host(&file.0);
    let mut provider = open(port, &ca_pem);
    let layouts = provider.layouts().unwrap();
    let tasks = layouts.iter().find(|l| l.name == "Tasks").unwrap().id;
    let notes = layouts.iter().find(|l| l.name == "Notes").unwrap().id;
    // A picture deliberately stored as a file, with a supporting stream.
    // Re-typing by the extension would turn FILE into PNGf and lose SIGN.
    let original = Container {
        path_type: Some("file"),
        streams: vec![
            (om_data::calc::StreamType::FILE, fixture::png()),
            (
                om_data::calc::StreamType(*b"SIGN"),
                b"supporting data".to_vec(),
            ),
        ],
        ..Container::file("photo.png", Vec::new())
    };
    let reference = provider
        .container_reference(tasks, "Photo", &original)
        .unwrap()
        .unwrap();
    let member = om_data::containers::reference(&reference).unwrap();
    let answer = provider
        .calc_data(&[CalcNeed::Member {
            table: TABLE,
            member: member.clone(),
        }])
        .unwrap();
    let source = answer.library.members[&(TABLE, member.clone())]
        .as_ref()
        .unwrap()
        .value(TABLE, &member);
    let copied = provider
        .container_reference(notes, "Photo", &source)
        .unwrap()
        .unwrap();
    let copied_member = om_data::containers::reference(&copied).unwrap();
    let answer = provider
        .calc_data(&[CalcNeed::MemberData {
            table: fixture::NOTES,
            member: copied_member.clone(),
        }])
        .unwrap();
    let stored = answer.library.data[&(fixture::NOTES, copied_member)]
        .as_ref()
        .unwrap();
    assert!(stored.intact);
    assert_eq!(stored.data.text, original.text);
    assert_eq!(stored.data.path_type, original.path_type);
    assert_eq!(stored.data.streams, original.streams);
    provider.close();
}

#[test]
fn cross_table_copies_refuse_unavailable_external_streams() {
    let mut root = fixture::tree(&[], true);
    fixture::add_unavailable_external_photo(&mut root);
    fixture::add_field(&mut root, fixture::NOTES, 20, "Photo", 1, 6, 0, 0, 0);
    let file = TempFile::from_tree("containers-cross-table-external", &root);
    let (port, ca_pem) = host(&file.0);
    let mut provider = open(port, &ca_pem);
    let notes = provider
        .layouts()
        .unwrap()
        .iter()
        .find(|l| l.name == "Notes")
        .unwrap()
        .id;
    let member = serial::encode(PHOTO_MEMBER);
    let answer = provider
        .calc_data(&[CalcNeed::Member {
            table: TABLE,
            member: member.clone(),
        }])
        .unwrap();
    let source = answer.library.members[&(TABLE, member.clone())]
        .as_ref()
        .unwrap()
        .value(TABLE, &member);
    let error = provider
        .container_reference(notes, "Photo", &source)
        .unwrap_err();
    assert_eq!(error.code, Some(1225));
    let target = provider
        .calc_data(&[CalcNeed::MemberData {
            table: fixture::NOTES,
            member: serial::encode(1),
        }])
        .unwrap();
    assert!(target.library.data[&(fixture::NOTES, serial::encode(1))].is_none());
    provider.close();
}

#[test]
fn unwritable_container_fields_are_refused_before_uploading_or_counting_data() {
    let mut root = fixture::tree(&[], true);
    fixture::add_nonwritable_photo_fields(&mut root);
    let file = TempFile::from_tree("containers-unwritable", &root);
    let (port, ca_pem) = host(&file.0);
    let mut provider = open(port, &ca_pem);
    let tasks = tasks_layout(&mut provider);
    let mut counts = raw_session(port);
    let member = serial::encode(PHOTO_MEMBER);
    let answer = provider
        .calc_data(&[CalcNeed::Member {
            table: TABLE,
            member: member.clone(),
        }])
        .unwrap();
    let existing = answer.library.members[&(TABLE, member.clone())]
        .as_ref()
        .unwrap()
        .value(TABLE, &member);
    for name in ["GlobalPhoto", "ComputedPhoto"] {
        for value in [
            &Container::file("never-uploaded.txt", b"new data".to_vec()),
            &existing,
        ] {
            let error = provider
                .container_reference(tasks, name, value)
                .unwrap_err();
            assert_eq!(error.code, Some(201), "{name}");
            assert_eq!(count(&mut counts, 1), Some(1));
            assert_eq!(count(&mut counts, 2), None);
        }
    }
    let error = provider
        .adjust_container(tasks, "7F\u{e000}", 1)
        .unwrap_err();
    assert_eq!(error.code, Some(3));
    provider.close();
}

/// A folder for one test's connection recordings, removed afterwards.
struct Recordings(std::path::PathBuf);

impl Recordings {
    fn new(tag: &str) -> Recordings {
        let dir = std::env::temp_dir().join(format!("om-containers-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Recordings(dir)
    }
}

impl Drop for Recordings {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A download the server received of Tasks' library, `[T, 1f, 05, M, K]`:
/// member M and key K below it, empty for the whole member (its data,
/// containers.md 6.1).
type LibraryRead = (u64, Vec<u8>);

/// Every library download the server was asked for, in order, from the
/// client streams it recorded: op90 and op15 by their key path, op92 by
/// its parent's.
fn library_reads(dir: &Path) -> Vec<LibraryRead> {
    let mut connections: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    connections.sort();
    let (table, mut reads) = (uint_segment(TABLE), Vec::new());
    for connection in connections {
        let stream = std::fs::read(connection.join("c2s.bin")).unwrap_or_default();
        let mut at = 0;
        while let Ok((_, length)) = om_giop::parse_header(&stream[at..]) {
            let message = &stream[at..at + length];
            at += length;
            let Ok(request) = om_giop::Request::parse(message) else {
                continue;
            };
            if request.operation != om_protocol::op::PERFORM {
                continue;
            }
            let offset = om_protocol::request_body_offset(message.len(), request.body.len());
            let transaction = om_protocol::perform_transaction(request.body, offset).unwrap();
            let path = match transaction[0] {
                90 | 15 => {
                    om_protocol::parse_download_request(transaction)
                        .unwrap()
                        .key_path
                }
                92 => {
                    om_protocol::parse_download_list_request(transaction)
                        .unwrap()
                        .key_path
                }
                _ => continue,
            };
            if path.first() != Some(&table) || path.get(1).map(Vec::as_slice) != Some(&[0x1f]) {
                continue;
            }
            match path.as_slice() {
                [_, _, members, member, key @ ..] if members.as_slice() == [5] => {
                    reads.push((serial::decode(member).unwrap(), key.concat()));
                }
                // Anything else under the library (its header, a list of
                // members) is a read these tests do not expect.
                _ => reads.push((0, path.concat())),
            }
        }
    }
    reads
}

/// The [`library_reads`] after the first `before`, moving `before` past
/// them.
fn reads_since(dir: &Path, before: &mut usize) -> Vec<LibraryRead> {
    let reads = library_reads(dir);
    let new = reads[*before..].to_vec();
    *before = reads.len();
    new
}

/// The reads of `member`'s header: its MD5, then its `FNAM`.
fn header(member: u64) -> [LibraryRead; 2] {
    [(member, vec![4]), (member, b"FNAM".to_vec())]
}

/// The read of `member`'s data.
fn data(member: u64) -> LibraryRead {
    (member, Vec::new())
}

/// Tasks::Video and Tasks::Scan, container fields beside Photo. Record 1's
/// Video names member 2, [`VIDEO_SIZE`] bytes named `clip.mp4`; its Scan
/// names member 3, whose header reads but whose streams are stored
/// externally in a file the host does not have.
const VIDEO: u64 = 30;
const SCAN: u64 = 31;
const VIDEO_MEMBER: u64 = 2;
const SCAN_MEMBER: u64 = 3;
const VIDEO_SIZE: usize = 150_000;
/// Text fields whose edits run the calculations below.
const PICK: u64 = 32;
const TAG: u64 = 33;
const MODE: u64 = 39;
const HUSH: u64 = 40;
/// The calculations of [`media_file`]: field, name, kind (2 stored
/// calculation, 1 auto-entered), option word and formula.
const MEDIA_CALCULATIONS: [(u64, &str, u8, u32, &str); 5] = [
    // The file name only: the header of Photo.
    (
        34,
        "Caption",
        2,
        0,
        "Name & If ( not IsEmpty ( Photo ) ; \" / \" & \
         GetContainerAttribute ( Photo ; \"filename\" ) )",
    ),
    // The data of the one member the branch taken names.
    (
        35,
        "Sizes",
        2,
        0,
        "Case ( Pick = \"photo\" ; Length ( Photo ) & \"x\" & GetWidth ( Photo ) ; \
         Pick = \"video\" ; Length ( Video ) ; Pick = \"scan\" ; Length ( Scan ) ; \"\" )",
    ),
    // Auto-entered, replacing its value (0x88): Video's name only when
    // picked.
    (
        36,
        "Seen",
        1,
        0x88,
        "If ( Pick = \"video\" ; GetAsText ( Video ) ; Pick )",
    ),
    // IsValid catches an error: a read still pending would take the other
    // branch, into Video (Guard), or give a value without an error (Quiet).
    (
        37,
        "Guard",
        2,
        0,
        "If ( IsEmpty ( Mode ) ; \"\" ; \
         If ( IsValid ( Length ( Photo ) ) ; Length ( Photo ) ; Length ( Video ) ) )",
    ),
    (
        38,
        "Quiet",
        2,
        0,
        "If ( IsEmpty ( Hush ) ; \"\" ; \
         If ( IsValid ( Length ( Photo ) ) ; Length ( Photo ) ; \"none\" ) )",
    ),
];
/// Tag's validation (always, by calculation): a picked member's data must
/// be over 1000 bytes.
const TAG_VALIDATION: &str = "Case ( Self = \"photo\" ; Length ( Photo ) > 1000 ; \
     Self = \"video\" ; Length ( Video ) > 1000 ; 1 )";

fn video_data() -> Vec<u8> {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    (0..VIDEO_SIZE)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

/// The Tasks file with Photo ([`fixture::add_photo`]), Video, Scan, Pick,
/// Tag and [`MEDIA_CALCULATIONS`]; returns the MD5s of members 1 and 2.
fn media_file(tag: &str) -> (TempFile, [[u8; 16]; 2]) {
    use om_data::calc::{StreamType, Text};
    use om_data::containers;
    use om_hbam::OwnedNode;

    let mut root = fixture::tree(&[], true);
    fixture::add_photo(&mut root);
    let mut names = fixture::names();
    names.add_field(TABLE, fixture::PHOTO, "Photo");
    for (id, name) in [
        (VIDEO, "Video"),
        (SCAN, "Scan"),
        (PICK, "Pick"),
        (TAG, "Tag"),
        (MODE, "Mode"),
        (HUSH, "Hush"),
    ] {
        names.add_field(TABLE, id, name);
    }
    for (id, name, ..) in MEDIA_CALCULATIONS {
        names.add_field(TABLE, id, name);
    }
    fixture::add_field(&mut root, TABLE, VIDEO, "Video", 1, 6, 0, 0, 0);
    fixture::add_field(&mut root, TABLE, SCAN, "Scan", 1, 6, 0, 0, 0);
    fixture::add_field(&mut root, TABLE, PICK, "Pick", 1, 1, 0, 0, 0);
    fixture::add_field(&mut root, TABLE, TAG, "Tag", 1, 1, 0, 0, 0x401);
    fixture::add_field(&mut root, TABLE, MODE, "Mode", 1, 1, 0, 0, 0);
    fixture::add_field(&mut root, TABLE, HUSH, "Hush", 1, 1, 0, 0, 0);
    fixture::add_formula(
        &mut root,
        &names,
        (TABLE, OCCURRENCE),
        TAG,
        0x19,
        TAG_VALIDATION,
    );
    for (id, name, kind, options, text) in MEDIA_CALCULATIONS {
        fixture::add_field(&mut root, TABLE, id, name, kind, 1, 0, options, 0);
        fixture::add_formula(&mut root, &names, (TABLE, OCCURRENCE), id, 5, text);
    }

    let t = uint_segment(TABLE);
    let count = || OwnedNode::leaf(vec![0x12], 1u32.to_be_bytes().to_vec());
    let video = Container {
        text: Text::plain("clip.mp4"),
        streams: vec![(StreamType::FILE, video_data())],
        path_type: Some("file"),
        ..Container::default()
    };
    let (mut video_tree, video_md5) = containers::tree(&video).unwrap();
    video_tree.push(count());
    video_tree.sort_by(|a, b| a.segment.cmp(&b.segment));
    // Scan: FNAM and a remote stream whose file the host cannot read.
    let name = containers::fnam("image", "scan.png");
    let reference = b"unavailable-external-data";
    let mut directory = 2u16.to_be_bytes().to_vec();
    for (kind, len) in [(b"FNAM", name.len()), (b"pngF", reference.len())] {
        directory.extend(kind);
        directory.extend((len as u32).to_be_bytes());
    }
    let scan_md5 = containers::md5([(b"FNAM".as_slice(), name.as_slice())]);
    let scan_tree = vec![
        OwnedNode::leaf(vec![2], directory),
        OwnedNode::leaf(vec![4], scan_md5.to_vec()),
        count(),
        OwnedNode::leaf(b"FNAM".to_vec(), name),
        OwnedNode::leaf(b"pngF".to_vec(), reference.to_vec()),
    ];
    let photo_md5 = {
        let member = root
            .at(&[&t, &[0x1f], &[5], &serial::encode(PHOTO_MEMBER)])
            .unwrap();
        om_data::containers::MemberInfo::read(member).unwrap().md5
    };
    let mut index = Vec::new();
    for (id, tree, md5) in [
        (VIDEO_MEMBER, video_tree, video_md5),
        (SCAN_MEMBER, scan_tree, scan_md5),
    ] {
        let m = serial::encode(id);
        root.put(&[&t, &[0x1f], &[5], &m], OwnedNode::list(m.clone(), tree));
        index.push((md5, m));
    }
    // The library header lists all three, the last issued being 3.
    let members: Vec<(Vec<u8>, [u8; 16])> = [(PHOTO_MEMBER, photo_md5)]
        .into_iter()
        .map(|(id, md5)| (serial::encode(id), md5))
        .chain(index.iter().map(|(md5, m)| (m.clone(), *md5)))
        .collect();
    let last = serial::encode(SCAN_MEMBER);
    let mut by_md5: Vec<_> = members.iter().map(|(m, md5)| (*md5, m.clone())).collect();
    by_md5.sort();
    let header = vec![
        OwnedNode::leaf(vec![0x00], [vec![last.len() as u8], last].concat()),
        OwnedNode::leaf(vec![0x04], (members.len() as u32).to_be_bytes().to_vec()),
        OwnedNode::list(vec![0x06], Vec::new()),
        OwnedNode::list(
            vec![0x07],
            members
                .iter()
                .map(|(m, _)| OwnedNode::leaf(m.clone(), Vec::new()))
                .collect(),
        ),
        OwnedNode::list(
            vec![0x08],
            by_md5
                .into_iter()
                .map(|(md5, m)| OwnedNode::list(md5.to_vec(), vec![OwnedNode::leaf(m, Vec::new())]))
                .collect(),
        ),
    ];
    root.put(&[&t, &[0x1f], &[1]], OwnedNode::list(vec![1], header));
    for (field, member) in [(VIDEO, VIDEO_MEMBER), (SCAN, SCAN_MEMBER)] {
        root.put(
            &[&t, &[5], &serial::encode(1), &uint_segment(field)],
            om_scripts::text_node(&[], &containers::reference_text(&serial::encode(member))),
        );
    }
    (TempFile::from_tree(tag, &root), [photo_md5, video_md5])
}

fn changes(values: &[(&str, &str)]) -> FieldChanges {
    values
        .iter()
        .map(|(field, value)| (field.to_string(), Some(value.to_string())))
        .collect()
}

/// Field `field` of record `id` as the host now holds it.
fn stored(provider: &mut NativeProvider, layout: u32, id: i64, field: &str) -> Option<String> {
    let page = provider.records(layout, 0, 10).unwrap();
    let column = page.fields.iter().position(|f| f.name == field)?;
    page.records.iter().find(|r| r.id == id)?.values[column].clone()
}

fn tasks_layout(provider: &mut NativeProvider) -> u32 {
    provider
        .layouts()
        .unwrap()
        .into_iter()
        .find(|l| l.name == "Tasks")
        .unwrap()
        .id
}

/// A write's calculations and validations download only the members they
/// read, and of those the header for what reads the field and the data
/// for what reads its streams, once per write: a text edit whose
/// calculation shows Photo's file name reads Photo's header and nothing of
/// Video or Scan; a branch reads only its own member; data that cannot be
/// read refuses the one write that reads it and no other.
#[test]
fn write_rules_download_only_the_container_data_they_read() {
    let (file, _) = media_file("containers-lazy-writes");
    let recordings = Recordings::new("lazy-writes");
    let (port, ca_pem) = host_recording(&file.0, Some(&recordings.0));
    let mut provider = open(port, &ca_pem);
    let tasks = tasks_layout(&mut provider);
    let dir = recordings.0.as_path();
    let mut seen = library_reads(dir).len();

    // The file name only: Photo's header, no data, nothing of Video or
    // Scan.
    provider
        .update_record(tasks, 1, changes(&[("Name", "Renamed")]))
        .unwrap();
    assert_eq!(reads_since(dir, &mut seen), header(PHOTO_MEMBER));
    assert_eq!(
        stored(&mut provider, tasks, 1, "Caption").as_deref(),
        Some("Renamed / photo2.png")
    );
    seen = library_reads(dir).len();

    // Length and GetWidth of Photo: its data, downloaded once for both
    // (the header is the connection's already); the other branches' members
    // are not read.
    provider
        .update_record(tasks, 1, changes(&[("Pick", "photo")]))
        .unwrap();
    assert_eq!(reads_since(dir, &mut seen), [data(PHOTO_MEMBER)]);
    assert_eq!(
        stored(&mut provider, tasks, 1, "Sizes").as_deref(),
        Some("73x3")
    );
    assert_eq!(
        stored(&mut provider, tasks, 1, "Seen").as_deref(),
        Some("photo")
    );
    seen = library_reads(dir).len();

    // Video: its header once for the stored and the auto-entered
    // calculation, its data once.
    provider
        .update_record(tasks, 1, changes(&[("Pick", "video")]))
        .unwrap();
    let mut expected = header(VIDEO_MEMBER).to_vec();
    expected.push(data(VIDEO_MEMBER));
    assert_eq!(reads_since(dir, &mut seen), expected);
    assert_eq!(
        stored(&mut provider, tasks, 1, "Sizes"),
        Some(VIDEO_SIZE.to_string())
    );
    assert_eq!(
        stored(&mut provider, tasks, 1, "Seen").as_deref(),
        Some("clip.mp4")
    );
    seen = library_reads(dir).len();

    // Scan's data cannot be read: the write that reads it is refused and
    // changes nothing.
    let refused = provider
        .update_record(tasks, 1, changes(&[("Pick", "scan")]))
        .unwrap_err();
    assert_eq!(refused.code, Some(3), "{refused}");
    assert!(refused.to_string().contains("1225"), "{refused}");
    let mut expected = header(SCAN_MEMBER).to_vec();
    expected.push(data(SCAN_MEMBER));
    assert_eq!(reads_since(dir, &mut seen), expected);
    assert_eq!(
        stored(&mut provider, tasks, 1, "Pick").as_deref(),
        Some("video")
    );
    assert_eq!(
        stored(&mut provider, tasks, 1, "Sizes"),
        Some(VIDEO_SIZE.to_string())
    );
    seen = library_reads(dir).len();

    // A write that does not read Scan saves, the record still naming it.
    provider
        .update_record(tasks, 1, changes(&[("Name", "Again")]))
        .unwrap();
    assert_eq!(reads_since(dir, &mut seen), []);
    assert_eq!(
        stored(&mut provider, tasks, 1, "Caption").as_deref(),
        Some("Again / photo2.png")
    );
    provider.close();
}

/// New records and a transaction's commit compute their calculations on
/// the members they read and nothing else: a new record naming Video
/// without reading it never downloads it, and a commit over the record
/// naming the unreadable Scan saves unless a rule reads Scan.
#[test]
fn creates_and_transactions_read_only_the_containers_their_rules_use() {
    use om_data::runtime::TransactionOptions;

    let (file, [photo_md5, video_md5]) = media_file("containers-lazy-commits");
    let recordings = Recordings::new("lazy-commits");
    let (port, ca_pem) = host_recording(&file.0, Some(&recordings.0));
    let mut provider = open(port, &ca_pem);
    let tasks = tasks_layout(&mut provider);
    let dir = recordings.0.as_path();
    let mut held = |member| {
        let member = serial::encode(member);
        let answer = provider
            .calc_data(&[CalcNeed::Member {
                table: TABLE,
                member: member.clone(),
            }])
            .unwrap();
        answer.library.members[&(TABLE, member.clone())]
            .as_ref()
            .unwrap()
            .value(TABLE, &member)
    };
    let photo_value = held(PHOTO_MEMBER);
    let video_value = held(VIDEO_MEMBER);
    assert_eq!(photo_value.library.as_ref().unwrap().md5, photo_md5);
    assert_eq!(video_value.library.as_ref().unwrap().md5, video_md5);
    let photo = provider
        .container_reference(tasks, "Photo", &photo_value)
        .unwrap()
        .unwrap();
    let video = provider
        .container_reference(tasks, "Video", &video_value)
        .unwrap()
        .unwrap();
    let video_again = provider
        .container_reference(tasks, "Video", &video_value)
        .unwrap()
        .unwrap();
    // Start a fresh page boundary after acquiring the real session-owned
    // values, so the read-count assertions begin with unvalidated headers.
    provider
        .records_with_calculations(tasks, 0, 0, &[])
        .unwrap();
    let mut seen = library_reads(dir).len();

    // A new record: Photo's header and data, nothing of Video.
    let created = provider
        .create_record(
            tasks,
            changes(&[
                ("Name", "New"),
                ("Pick", "photo"),
                ("Photo", &photo),
                ("Video", &video),
            ]),
        )
        .unwrap();
    let mut expected = header(PHOTO_MEMBER).to_vec();
    expected.push(data(PHOTO_MEMBER));
    assert_eq!(reads_since(dir, &mut seen), expected);
    for (field, value) in [
        ("Caption", "New / photo2.png"),
        ("Sizes", "73x3"),
        ("Seen", "photo"),
    ] {
        assert_eq!(
            stored(&mut provider, tasks, created.id, field).as_deref(),
            Some(value),
            "{field}"
        );
    }
    seen = library_reads(dir).len();

    // A commit changing record 1, which names Scan, and adding a record
    // that reads Video: Video only.
    provider
        .begin_transaction(TransactionOptions::default())
        .unwrap();
    provider
        .update_record(tasks, 1, changes(&[("Pick", "photo")]))
        .unwrap();
    let added = provider
        .create_record(
            tasks,
            changes(&[("Name", "Held"), ("Pick", "video"), ("Video", &video_again)]),
        )
        .unwrap();
    provider.commit_transaction(false).unwrap();
    let mut expected = header(VIDEO_MEMBER).to_vec();
    expected.push(data(VIDEO_MEMBER));
    assert_eq!(reads_since(dir, &mut seen), expected);
    assert_eq!(
        stored(&mut provider, tasks, 1, "Sizes").as_deref(),
        Some("73x3")
    );
    assert_eq!(
        stored(&mut provider, tasks, added.id, "Sizes"),
        Some(VIDEO_SIZE.to_string())
    );
    assert_eq!(
        stored(&mut provider, tasks, added.id, "Seen").as_deref(),
        Some("clip.mp4")
    );
    seen = library_reads(dir).len();

    // A commit whose rules read Scan is refused whole.
    provider
        .begin_transaction(TransactionOptions::default())
        .unwrap();
    provider
        .update_record(tasks, 1, changes(&[("Pick", "scan")]))
        .unwrap();
    let refused = provider.commit_transaction(false).unwrap_err();
    assert!(refused.to_string().contains("1225"), "{refused}");
    let mut expected = header(SCAN_MEMBER).to_vec();
    expected.push(data(SCAN_MEMBER));
    assert_eq!(reads_since(dir, &mut seen), expected);
    assert_eq!(
        stored(&mut provider, tasks, 1, "Pick").as_deref(),
        Some("photo")
    );
    provider.close();
}

/// A validation calculation sees the data it reads, not a read still
/// pending: Photo's 73 bytes fail "over 1000 bytes" (507), Video's 150 000
/// pass, and a value whose branch reads no container reads nothing.
#[test]
fn validations_read_the_container_data_they_check() {
    let (file, _) = media_file("containers-lazy-validation");
    let recordings = Recordings::new("lazy-validation");
    let (port, ca_pem) = host_recording(&file.0, Some(&recordings.0));
    let mut provider = open(port, &ca_pem);
    let tasks = tasks_layout(&mut provider);
    let dir = recordings.0.as_path();
    let mut seen = library_reads(dir).len();

    provider
        .update_record(tasks, 1, changes(&[("Tag", "plain")]))
        .unwrap();
    assert_eq!(reads_since(dir, &mut seen), []);

    let refused = provider
        .update_record(tasks, 1, changes(&[("Tag", "photo")]))
        .unwrap_err();
    assert_eq!(refused.code, Some(507), "{refused}");
    let mut expected = header(PHOTO_MEMBER).to_vec();
    expected.push(data(PHOTO_MEMBER));
    assert_eq!(reads_since(dir, &mut seen), expected);
    assert_eq!(
        stored(&mut provider, tasks, 1, "Tag").as_deref(),
        Some("plain")
    );
    seen = library_reads(dir).len();

    provider
        .update_record(tasks, 1, changes(&[("Tag", "video")]))
        .unwrap();
    let mut expected = header(VIDEO_MEMBER).to_vec();
    expected.push(data(VIDEO_MEMBER));
    assert_eq!(reads_since(dir, &mut seen), expected);
    assert_eq!(
        stored(&mut provider, tasks, 1, "Tag").as_deref(),
        Some("video")
    );
    provider.close();
}

/// A run of a write's rules that lacked a read is never kept, nor does it
/// download for the branches a pending read's `?` led it into: Quiet and
/// Guard read Photo through `IsValid`, which catches the pending error, on
/// connections that have read nothing yet.
#[test]
fn a_pending_read_never_decides_a_write_or_downloads_for_it() {
    let (file, _) = media_file("containers-lazy-pending");
    let recordings = Recordings::new("lazy-pending");
    let (port, ca_pem) = host_recording(&file.0, Some(&recordings.0));
    let dir = recordings.0.as_path();
    let mut expected = header(PHOTO_MEMBER).to_vec();
    expected.push(data(PHOTO_MEMBER));
    for (field, trigger) in [("Quiet", "Hush"), ("Guard", "Mode")] {
        let mut provider = open(port, &ca_pem);
        let tasks = tasks_layout(&mut provider);
        let mut seen = library_reads(dir).len();
        provider
            .update_record(tasks, 1, changes(&[(trigger, "go")]))
            .unwrap();
        assert_eq!(reads_since(dir, &mut seen), expected, "{field}");
        assert_eq!(
            stored(&mut provider, tasks, 1, field).as_deref(),
            Some("73"),
            "{field}"
        );
        provider.close();
    }
}

// Stored decryption and refused replacement regressions; synthetic loopback files.
fn container_function(name: &str, args: &[om_data::calc::Value]) -> om_data::calc::Value {
    use om_data::calc::{
        functions::{lookup, Implementation},
        Ctx, Date, Locale,
    };
    let Implementation::Pure(f) = lookup(name).unwrap().implementation else {
        panic!("pure")
    };
    f(
        args,
        &Ctx::new(&Locale::en_us(), Date::from_ymd(2026, 10, 31).unwrap()),
    )
    .unwrap()
}

#[test]
fn stored_decryption_loads_the_executed_container_body() {
    use om_data::calc::Value;
    let file = file("stored-decryption");
    let (port, ca_pem) = host(&file.0);
    let mut provider = open(port, &ca_pem);
    let layout = tasks_layout(&mut provider);
    let args = [
        Value::text("synthetic round trip"),
        Value::text("fixture-key"),
    ];
    let encrypted = container_function("CryptEncrypt", &args);
    let Value::Container(encrypted) = encrypted else {
        panic!("container")
    };
    let direct = container_function(
        "CryptDecrypt",
        &[Value::Container(encrypted.clone()), args[1].clone()],
    );
    assert!(
        matches!(direct, Value::Text(t) if t.as_str() == "synthetic round trip"),
        "direct-value control"
    );
    let reference = provider
        .container_reference(layout, "Photo", &encrypted)
        .unwrap()
        .unwrap();
    provider
        .update_record(layout, 1, photo(Some(&reference)))
        .unwrap();
    let cache = RefCell::new(CalcData::default());
    let (unexecuted, needs) = evaluate(
        &mut provider,
        &cache,
        "If ( 0 ; CryptDecrypt ( Tasks::Photo ; \"fixture-key\" ) ; \"skipped\" )",
    );
    assert_eq!(unexecuted, "skipped");
    assert!(!needs
        .iter()
        .any(|n| matches!(n, CalcNeed::MemberData { .. })));
    let (actual, needs) = evaluate(
        &mut provider,
        &cache,
        "CryptDecrypt ( Tasks::Photo ; \"fixture-key\" )",
    );
    eprintln!("stored decrypt = {actual:?}; needs = {needs:?}");
    provider.close();
    assert_eq!(
        actual, "synthetic round trip",
        "stored data must match the direct-value control"
    );
    assert!(needs
        .iter()
        .any(|n| matches!(n, CalcNeed::MemberData { .. })));
}

fn refused_replacement(transaction: bool) {
    let file = file(if transaction {
        "refused-tx-refusal"
    } else {
        "refused-row-refusal"
    });
    let (port, _) = host(&file.0);
    let relay = relay::Relay::start(port, std::time::Duration::ZERO);
    let mut provider = open(relay.port, &relay.ca_pem);
    let layout = tasks_layout(&mut provider);
    if transaction {
        provider
            .begin_transaction(om_data::runtime::TransactionOptions::default())
            .unwrap();
    }
    let new = provider
        .container_reference(
            layout,
            "Photo",
            &Container::file("replacement.txt", b"replacement".to_vec()),
        )
        .unwrap()
        .unwrap();
    relay.refuse_commits();
    let changed = provider.update_record(layout, 1, photo(Some(&new)));
    let result = if transaction {
        changed.unwrap();
        provider.commit_transaction(false)
    } else {
        changed.map(|_| ())
    };
    assert!(
        result.is_err(),
        "verification must detect the refused commit"
    );
    let mut counts = raw_session(port);
    let t = uint_segment(TABLE);
    let path = [t, vec![5], serial::encode(1), uint_segment(fixture::PHOTO)];
    let reference = om_scsu::decode_scsu(&stored_bytes(&mut counts, &path)).unwrap();
    let old_count = count(&mut counts, 1);
    let new_count = count(&mut counts, 2);
    eprintln!("transaction={transaction}; error={result:?}; stored={reference:?}; old={old_count:?}; new={new_count:?}");
    assert_eq!(
        reference,
        om_data::containers::reference_text(&serial::encode(1)),
        "refusal left the original record intact"
    );
    assert_eq!(new_count, Some(1), "unknown outcome retains acquisition");
    provider.close();
    assert_eq!(
        old_count,
        Some(1),
        "failed verification must retain data still owned by the stored original"
    );
}

#[test]
fn refused_single_replacement_preserves_the_original_member() {
    refused_replacement(false);
}
#[test]
fn refused_transaction_replacement_preserves_the_original_member() {
    refused_replacement(true);
}

#[test]
fn base64_decryption_loads_a_stored_container_key() {
    use om_data::calc::Value;
    let key = Container::file("fixture.key", b"fixture-key".to_vec());
    let encrypted = container_function(
        "CryptEncryptBase64",
        &[
            Value::text("synthetic key round trip"),
            Value::Container(key.clone()),
        ],
    );
    let Value::Text(blob) = encrypted else {
        panic!("base64 text")
    };
    let control = container_function(
        "CryptDecryptBase64",
        &[Value::Text(blob.clone()), Value::Container(key.clone())],
    );
    assert!(matches!(control,Value::Text(t) if t.as_str()=="synthetic key round trip"));
    let file = file("stored-decrypt-key");
    let (port, ca_pem) = host(&file.0);
    let mut provider = open(port, &ca_pem);
    let layout = tasks_layout(&mut provider);
    let reference = provider
        .container_reference(layout, "Photo", &key)
        .unwrap()
        .unwrap();
    provider
        .update_record(layout, 1, photo(Some(&reference)))
        .unwrap();
    let cache = RefCell::new(CalcData::default());
    let expression = format!(
        "CryptDecryptBase64 ( \"{}\" ; Tasks::Photo )",
        blob.as_str()
    );
    let (actual, needs) = evaluate(&mut provider, &cache, &expression);
    eprintln!("stored key decrypt = {actual:?}; needs = {needs:?}");
    provider.close();
    assert_eq!(actual, "synthetic key round trip");
    assert!(needs
        .iter()
        .any(|n| matches!(n, CalcNeed::MemberData { .. })));
}

#[test]
fn script_create_uses_container_rule_loading() {
    let (file, _) = media_file("review-script-create-rules");
    let (port, ca) = host(&file.0);
    let mut provider = open(port, &ca);
    let layout = tasks_layout(&mut provider);
    let reference = om_data::containers::reference_text(&serial::encode(PHOTO_MEMBER));
    provider.adjust_container(layout, &reference, 1).unwrap();
    let direct = provider
        .create_record(
            layout,
            changes(&[("Name", "Copied"), ("Pick", "photo"), ("Photo", &reference)]),
        )
        .unwrap();
    let expected: Vec<_> = ["Caption", "Sizes", "Seen"]
        .into_iter()
        .map(|field| (field, stored(&mut provider, layout, direct.id, field)))
        .collect();
    assert_eq!(
        expected,
        [
            ("Caption", Some("Copied / photo2.png".into())),
            ("Sizes", Some("73x3".into())),
            ("Seen", Some("photo".into()))
        ]
    );
    provider.adjust_container(layout, &reference, 1).unwrap();
    let result = provider.write_record(&om_data::ScriptWrite {
        layout,
        record: None,
        cells: [
            (fixture::NAME, Some("Copied".into())),
            (PICK, Some("photo".into())),
            (fixture::PHOTO, Some(reference)),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    });
    let created =
        result.expect("the public script creation path supports stored container calculations");
    for (field, expected) in expected {
        let actual = stored(&mut provider, layout, created.id, field);
        assert_eq!(
            actual, expected,
            "script creation must compute stored fields from the container data"
        );
    }
    provider.close();
}

#[test]
fn script_create_refuses_an_unavailable_executed_body() {
    let (file, _) = media_file("review-script-create-unavailable");
    let (port, ca) = host(&file.0);
    let mut provider = open(port, &ca);
    let layout = tasks_layout(&mut provider);
    let reference = om_data::containers::reference_text(&serial::encode(SCAN_MEMBER));
    provider.adjust_container(layout, &reference, 1).unwrap();
    let result = provider.write_record(&om_data::ScriptWrite {
        layout,
        record: None,
        cells: [
            (fixture::NAME, Some("Unreadable".into())),
            (PICK, Some("scan".into())),
            (SCAN, Some(reference)),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    });
    let err = result.expect_err(
        "unavailable executed data must refuse the creation before saving guessed values",
    );
    assert!(err.to_string().contains("1225"), "{err:?}");
    provider.close();
}

#[test]
fn script_create_keeps_data_entry_validation_options() {
    let (file, _) = media_file("container-script-entry");
    let mut root = om_hbam::HbamFile::open(&file.0)
        .unwrap()
        .key_tree()
        .unwrap()
        .root
        .to_owned_node();
    fixture::set_field_validation(&mut root, TABLE, fixture::CODE, 0x5);
    let file = TempFile::from_tree("container-script-entry-only", &root);
    let (port, ca) = host(&file.0);
    let mut provider = open(port, &ca);
    let layout = tasks_layout(&mut provider);
    let direct = provider
        .create_record(
            layout,
            changes(&[("Name", "Entry control"), ("Code", "TOOLONG")]),
        )
        .unwrap_err();
    assert_eq!(
        direct.code,
        Some(507),
        "ordinary data entry checks this validation"
    );
    let reference = om_data::containers::reference_text(&serial::encode(PHOTO_MEMBER));
    provider.adjust_container(layout, &reference, 1).unwrap();
    let record = provider
        .write_record(&om_data::ScriptWrite {
            layout,
            record: None,
            skip_validation: true,
            cells: [
                (fixture::NAME, Some("Script entry".into())),
                (fixture::CODE, Some("TOOLONG".into())),
                (PICK, Some("photo".into())),
                (fixture::PHOTO, Some(reference)),
            ]
            .into(),
            ..Default::default()
        })
        .expect(
            "script skip option skips data-entry-only validation while retaining container rules",
        );
    assert_eq!(
        stored(&mut provider, layout, record.id, "Code").as_deref(),
        Some("TOOLONG")
    );
    assert_eq!(
        stored(&mut provider, layout, record.id, "Sizes").as_deref(),
        Some("73x3")
    );
    provider.close();
}
