//! End to end: container fields in server scripts (containers.md 1.4, 2,
//! 5, 7.3; execute-sql.md 8.1; script-runtime.md "Implementation notes
//! (PSOS)").
//!
//! The Tasks file of `fixtures/tasks.rs` with a container field Photo whose
//! record 1 holds library member 1: a 3 x 2 PNG named `photo2.png` (count
//! 1). Calculations read a container field's data from the table's
//! library, as FileMaker does: its text is the stored file name, its data
//! the member's streams. Records a script deletes release their container
//! data in the same saved commit, as FileMaker Pro's op87 does for a
//! client's delete.

#[path = "fixtures/tasks.rs"]
mod fixture;
#[path = "fixtures/server.rs"]
mod server;
#[path = "fixtures/transfer.rs"]
mod transfer;

use std::path::PathBuf;

use fixture::{
    go_to_record, on, png, script, TempFile, NAME, OCCURRENCE, PHOTO, PHOTO_MEMBER as MEMBER, TABLE,
};
use md5::{Digest, Md5};
use om_hbam::slot::uint_segment;
use om_hbam::{serial, OwnedNode};
use om_scripts::{Calculation, FieldSpec, Script, ScriptStep, StoredKey};
use om_session::Session;

/// `text` compiled in the Tasks context, with Photo among the names.
fn calc(text: &str) -> Calculation {
    let names = fixture::photo_names();
    let mut ctx = om_formula::Context::new(&names);
    ctx.occurrence = Some(OCCURRENCE);
    let compiled = om_formula::compile(text, &ctx).unwrap_or_else(|e| panic!("{text}: {e:?}"));
    Calculation::new(
        Some(om_formula::formatted::encode(&compiled.tokens)),
        om_formula::polish::encode(&compiled.expr),
    )
}

fn exit_with(text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(103);
    step.set_calculation(0, Some(&calc(text)));
    step
}

fn set_variable(name: &str, text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(141);
    step.set_text(0, Some(name));
    step.set_calculation(1, Some(&calc(text)));
    step
}

/// Set Field [Tasks::Photo; text].
fn set_photo(text: &str) -> ScriptStep {
    let mut step = ScriptStep::new(76);
    step.set_calculation(0, Some(&calc(text)));
    step.set_field_spec(
        0,
        Some(FieldSpec::new(
            StoredKey::from_id(OCCURRENCE).unwrap(),
            StoredKey::from_id(PHOTO).unwrap(),
            0,
        )),
    );
    step
}

/// Insert from URL [Select; With dialog: Off; Tasks::field; url].
fn insert_from_url(field: u64, url: &str) -> ScriptStep {
    let mut step = ScriptStep::new(160);
    step.set_calculation(0, Some(&calc(url)));
    step.set_field_spec(
        0,
        Some(FieldSpec::new(
            StoredKey::from_id(OCCURRENCE).unwrap(),
            StoredKey::from_id(field).unwrap(),
            0,
        )),
    );
    step.flags |= om_scripts::flag::SELECT_ALL | om_scripts::flag::NO_INTERACT;
    step
}

/// Export Field Contents [Tasks::Photo; "$p"; create folders].
fn export_photo() -> ScriptStep {
    let mut step = ScriptStep::new(132);
    step.set_field_spec(
        0,
        Some(FieldSpec::new(
            StoredKey::from_id(OCCURRENCE).unwrap(),
            StoredKey::from_id(PHOTO).unwrap(),
            0,
        )),
    );
    step.flags |= om_scripts::flag::OPTION2;
    transfer::with_paths(step, &["$p"])
}

/// `Base64Decode` of "hello" named `a.txt`, of the PNG named `red.png`.
const HELLO: &str = "Base64Decode ( \"aGVsbG8=\" ; \"a.txt\" )";
const RED: &str = "Base64Decode ( \"iVBORw0KGgoAAAANSUhEUgAAAAMAAAACCAIAAAASFvFNAAAAEElEQVQI12P8zwAFTDAKADMpAQM7sWkeAAAAAElFTkSuQmCC\" ; \"red.png\" )";
/// Stored calculations and an auto-enter calculation reading Photo
/// ([`fixture::PHOTO_CALCULATIONS`]).
const CALCS: &str = "Tasks::PhotoName & \"~\" & Tasks::PhotoLength & \"~\" & Tasks::PhotoB64 \
     & \"~\" & Tasks::PhotoAE";

/// What the Set Field scripts read back.
const BACK: &str = "GetAsText ( Tasks::Photo ) & \"|\" & Length ( Tasks::Photo ) & \"|\" & \
     Base64Encode ( Tasks::Photo ) & \"|\" & IsEmpty ( Tasks::Photo )";

/// Steps by id: New Record 7, Duplicate Record 8, Delete Record 9, Delete
/// All Records 10, Truncate Table 182, Open Transaction 205, Commit
/// Transaction 206, Revert Transaction 207.
fn step(id: u16) -> ScriptStep {
    ScriptStep::new(id)
}

/// What the scripts read of record 1's photo: its text, size, name
/// attribute, byte length, Base64 length, MD5, verification, a thumbnail's
/// name, the ExecuteSQL column and the number.
const READ: &str = "GetAsText ( Tasks::Photo ) & \"|\" & GetWidth ( Tasks::Photo ) & \"x\" & \
     GetHeight ( Tasks::Photo ) & \"|\" & GetContainerAttribute ( Tasks::Photo ; \"filename\" ) & \
     \"|\" & Length ( Tasks::Photo ) & \"|\" & Length ( Base64EncodeRFC ( 4648 ; Tasks::Photo ) ) & \
     \"|\" & HexEncode ( CryptDigest ( Tasks::Photo ; \"MD5\" ) ) & \"|\" & VerifyContainer ( \
     Tasks::Photo ) & \"|\" & GetAsText ( GetThumbnail ( Tasks::Photo ; 2 ; 2 ) ) & \"|\" & \
     ExecuteSQL ( \"SELECT Photo FROM Tasks WHERE Name = 'Write tests'\" ; \"\" ; \"\" ) & \"|\" & \
     GetAsNumber ( Tasks::Photo ) & \"|\" & IsEmpty ( Tasks::Photo )";

fn scripts() -> Vec<Script> {
    vec![
        script(
            7,
            "Read",
            vec![on(86, true), go_to_record(1), exit_with(READ)],
        ),
        script(
            14,
            "Read last",
            vec![on(86, true), go_to_record(2), exit_with(READ)],
        ),
        script(
            15,
            "Delete last",
            vec![
                on(86, true),
                go_to_record(2),
                step(9),
                exit_with("Get ( LastError ) & \"|\" & Get ( TotalRecordCount )"),
            ],
        ),
        script(
            8,
            "Delete",
            vec![
                on(86, true),
                go_to_record(1),
                step(9),
                exit_with("Get ( LastError ) & \"|\" & Get ( TotalRecordCount )"),
            ],
        ),
        script(
            9,
            "Duplicate",
            vec![
                on(86, true),
                go_to_record(1),
                step(8),
                ScriptStep::new(75),
                exit_with("Get ( LastError ) & \"|\" & Get ( TotalRecordCount )"),
            ],
        ),
        script(
            10,
            "Truncate",
            vec![
                on(86, true),
                step(182),
                exit_with("Get ( LastError ) & \"|\" & Get ( TotalRecordCount )"),
            ],
        ),
        script(
            11,
            "Delete reverted",
            vec![
                on(86, true),
                step(205),
                go_to_record(1),
                step(9),
                step(207),
                step(206),
                exit_with("Get ( LastError ) & \"|\" & Get ( TotalRecordCount )"),
            ],
        ),
        script(
            12,
            "Delete in transaction",
            vec![
                on(86, true),
                step(205),
                go_to_record(1),
                step(9),
                set_variable("$open", "Get ( TotalRecordCount )"),
                step(206),
                exit_with("Get ( LastError ) & \"|\" & $open & \"|\" & Get ( TotalRecordCount )"),
            ],
        ),
        script(
            13,
            "Delete all",
            vec![
                on(86, true),
                step(10),
                exit_with("Get ( LastError ) & \"|\" & Get ( TotalRecordCount )"),
            ],
        ),
        script(
            16,
            "Set data",
            vec![
                on(86, true),
                step(7),
                set_photo(HELLO),
                set_variable("$open", BACK),
                ScriptStep::new(75),
                exit_with(&format!(
                    "Get ( LastError ) & \"|\" & $open & \"|\" & {BACK} & \"|\" & Get ( RecordID )"
                )),
            ],
        ),
        script(
            17,
            "Set picture",
            vec![
                on(86, true),
                step(7),
                set_photo(RED),
                ScriptStep::new(75),
                exit_with(
                    "Get ( LastError ) & \"|\" & GetAsText ( Tasks::Photo ) & \"|\" & \
                     GetWidth ( Tasks::Photo ) & \"x\" & GetHeight ( Tasks::Photo ) & \"|\" & \
                     GetContainerAttribute ( Tasks::Photo ; \"internalSize\" ) & \"|\" & \
                     VerifyContainer ( Tasks::Photo )",
                ),
            ],
        ),
        script(
            18,
            "Copy",
            vec![
                on(86, true),
                go_to_record(1),
                set_variable("$c", "Tasks::Photo"),
                step(7),
                set_photo("$c"),
                ScriptStep::new(75),
                exit_with("Get ( LastError ) & \"|\" & GetAsText ( Tasks::Photo ) & \"|\" & ( Tasks::Photo = $c )"),
            ],
        ),
        script(
            19,
            "Text",
            vec![
                on(86, true),
                step(7),
                set_photo("\"hello\""),
                ScriptStep::new(75),
                exit_with(&format!("Get ( LastError ) & \"|\" & {BACK}")),
            ],
        ),
        script(
            20,
            "Clear",
            vec![
                on(86, true),
                go_to_record(1),
                set_photo("\"\""),
                ScriptStep::new(75),
                exit_with(&format!("Get ( LastError ) & \"|\" & {BACK}")),
            ],
        ),
        script(
            21,
            "Twice",
            vec![
                on(86, true),
                step(7),
                set_photo(HELLO),
                set_photo(HELLO),
                ScriptStep::new(75),
                exit_with("Get ( LastError )"),
            ],
        ),
        script(
            22,
            "Reverted",
            vec![
                on(86, true),
                step(7),
                set_photo(HELLO),
                step(51),
                exit_with("Get ( LastError ) & \"|\" & Get ( TotalRecordCount )"),
            ],
        ),
        script(
            23,
            "In a transaction",
            vec![
                on(86, true),
                step(205),
                step(7),
                set_photo(HELLO),
                step(7),
                set_photo(HELLO),
                step(206),
                exit_with("Get ( LastError ) & \"|\" & Get ( TotalRecordCount )"),
            ],
        ),
        script(
            25,
            "Export",
            vec![
                on(86, true),
                go_to_record(1),
                set_variable("$p", "Get ( DocumentsPath ) & Get ( ScriptParameter )"),
                export_photo(),
                exit_with("Get ( LastError )"),
            ],
        ),
        script(
            26,
            "Insert from URL",
            vec![
                on(86, true),
                step(7),
                insert_from_url(PHOTO, "Get ( ScriptParameter )"),
                set_variable("$e", "Get ( LastError )"),
                ScriptStep::new(75),
                exit_with(
                    "$e & \"|\" & GetAsText ( Tasks::Photo ) & \"|\" & GetWidth ( Tasks::Photo ) \
                     & \"x\" & GetHeight ( Tasks::Photo ) & \"|\" & \
                     GetContainerAttribute ( Tasks::Photo ; \"internalSize\" ) & \"|\" & Get ( RecordID )",
                ),
            ],
        ),
        script(
            27,
            "Insert text from URL",
            vec![
                on(86, true),
                step(7),
                insert_from_url(NAME, "Get ( ScriptParameter )"),
                set_variable("$e", "Get ( LastError )"),
                ScriptStep::new(75),
                exit_with("$e & \"|\" & Tasks::Name"),
            ],
        ),
        script(
            28,
            "Calculations",
            vec![
                on(86, true),
                step(7),
                set_photo(HELLO),
                ScriptStep::new(75),
                set_variable("$data", CALCS),
                set_photo("\"hello\""),
                ScriptStep::new(75),
                set_variable("$text", CALCS),
                set_photo("\"\""),
                ScriptStep::new(75),
                exit_with(&format!("$data & \"/\" & $text & \"/\" & {CALCS}")),
            ],
        ),
        script(
            24,
            "Transaction reverted",
            vec![
                on(86, true),
                step(205),
                step(7),
                set_photo(RED),
                step(207),
                step(206),
                exit_with("Get ( LastError ) & \"|\" & Get ( TotalRecordCount )"),
            ],
        ),
    ]
}

/// Tasks with Photo and record 1's picture ([`fixture::add_photo`]).
fn with_photo() -> OwnedNode {
    let mut root = fixture::tree(&scripts(), true);
    fixture::add_photo(&mut root);
    root
}

/// The reference count of member 1 of Tasks' library, `None` when the
/// member is gone; and the header's member count.
fn library_state(session: &mut Session) -> (Option<u32>, Option<u32>) {
    let (t, m) = (uint_segment(TABLE), serial::encode(MEMBER));
    let leaf = |session: &mut Session, path: &[&[u8]]| {
        let reply = session.download(path).unwrap();
        (reply.node_state == om_protocol::NodeState::Terminal)
            .then(|| Some(u32::from_be_bytes(reply.value.as_slice().try_into().ok()?)))
            .flatten()
    };
    (
        leaf(session, &[&t, &[0x1f], &[5], &m, &[0x12]]),
        leaf(session, &[&t, &[0x1f], &[1], &[0x04]]),
    )
}

/// The reference count of `member` of Tasks' library (`None`: gone) and the
/// header's last issued member id and member count.
fn member_state(session: &mut Session, member: u64) -> (Option<u32>, Vec<u8>, Option<u32>) {
    let (t, m) = (uint_segment(TABLE), serial::encode(member));
    let value = |session: &mut Session, path: &[&[u8]]| {
        let reply = session.download(path).unwrap();
        (reply.node_state == om_protocol::NodeState::Terminal).then_some(reply.value)
    };
    let count = |v: Option<Vec<u8>>| Some(u32::from_be_bytes(v?.as_slice().try_into().ok()?));
    (
        count(value(session, &[&t, &[0x1f], &[5], &m, &[0x12]])),
        value(session, &[&t, &[0x1f], &[1], &[0x00]]).unwrap_or_default(),
        count(value(session, &[&t, &[0x1f], &[1], &[0x04]])),
    )
}

/// Saves of `file` fail while this lives (as in `saves.rs`).
struct BlockedSaves(PathBuf);

impl BlockedSaves {
    fn new(file: &TempFile) -> Self {
        let mut path = file.0.as_os_str().to_owned();
        path.push(".omsave");
        let path = PathBuf::from(path);
        std::fs::create_dir(&path).unwrap();
        BlockedSaves(path)
    }
}

impl Drop for BlockedSaves {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

fn run(client: &mut Session, script: u64) -> String {
    let reply = client.perform_script_on_server(script, "", true).unwrap();
    assert_eq!(reply.error, 0, "script {script}: {reply:?}");
    reply.result
}

#[test]
fn calculations_read_container_data_from_the_library() {
    let file = TempFile::from_tree("containers-read", &with_photo());
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "containers-read");
    let digest: String = Md5::digest(png())
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect();
    // The text is the stored file name (GetAsText, ExecuteSQL), the data
    // the member's streams (its size, the encoding and digest functions);
    // embedded data is not verified (`?`, script oracle s34); container
    // data is 0 as a number whatever its name.
    assert_eq!(
        run(&mut client, 7),
        format!("photo2.png|3x2|photo2.png|73|100|{digest}|?|photo2.png|photo2.png|0|0")
    );
    client.disconnect().unwrap();
}

#[test]
fn deleted_records_release_their_container_data() {
    let file = TempFile::from_tree("containers-delete", &with_photo());
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "containers-delete");
    assert_eq!(library_state(&mut client), (Some(1), Some(1)));

    // A delete that cannot be saved keeps the record and its data.
    let blocked = BlockedSaves::new(&file);
    let reply = client.perform_script_on_server(8, "", true).unwrap();
    assert_eq!((reply.error, reply.result.as_str()), (800, "800|2"));
    assert_eq!(library_state(&mut client), (Some(1), Some(1)));
    drop(blocked);

    // Reverted in a transaction: nothing is released.
    assert_eq!(run(&mut client, 11), "0|2");
    assert_eq!(library_state(&mut client), (Some(1), Some(1)));

    // Deleted in a transaction: released when it commits. The member goes
    // with its last reference, and the header no longer lists it.
    assert_eq!(run(&mut client, 12), "0|1|1");
    assert_eq!(library_state(&mut client), (None, Some(0)));
    client.disconnect().unwrap();
}

#[test]
fn a_copy_keeps_the_data_its_original_released() {
    let file = TempFile::from_tree("containers-copy", &with_photo());
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "containers-copy");
    // The copy takes a reference; deleting the original releases its own.
    assert_eq!(run(&mut client, 9), "0|3");
    assert_eq!(library_state(&mut client), (Some(2), Some(1)));
    assert_eq!(run(&mut client, 8), "0|2");
    assert_eq!(library_state(&mut client), (Some(1), Some(1)));
    let records = server::records_of(&mut client, TABLE);
    let ids: Vec<u64> = records.iter().map(|(id, _)| *id).collect();
    assert_eq!(ids, [2, 3]);
    assert_eq!(server::cell(&records, 3, PHOTO), Some("01\u{e000}"));
    // The copy reads the data as the original did.
    let digest: String = Md5::digest(png())
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect();
    assert_eq!(
        run(&mut client, 14),
        format!("photo2.png|3x2|photo2.png|73|100|{digest}|?|photo2.png|photo2.png|0|0")
    );
    // Its delete releases the last reference.
    assert_eq!(run(&mut client, 15), "0|1");
    assert_eq!(library_state(&mut client), (None, Some(0)));
    let _ = NAME;
    client.disconnect().unwrap();
}

#[test]
fn truncating_and_deleting_all_records_release_every_reference() {
    for (script, label) in [(10, "truncate"), (13, "delete-all")] {
        let file = TempFile::from_tree(&format!("containers-{label}"), &with_photo());
        let port = server::start(&file);
        let (mut client, _) = server::logged_in(port, label);
        assert_eq!(run(&mut client, 9), "0|3");
        assert_eq!(library_state(&mut client), (Some(2), Some(1)));
        assert_eq!(run(&mut client, script), "0|0", "{label}");
        // The table is empty: its whole library goes, header and all, as
        // on FileMaker Server's engine (s34-containers-release).
        assert_eq!(library_state(&mut client), (None, None), "{label}");
        client.disconnect().unwrap();
    }
}

/// Set Field puts data into the library with its commit, as FileMaker
/// Server's engine stores it (script oracle s34-containers-setfield and
/// -extensions): a new member counted once, the same data deduplicated, a
/// copy of the table's value one more reference, text kept as text, a
/// cleared value released; nothing for a revert or a failed save.
#[test]
fn set_field_stores_container_data_with_the_commit() {
    let file = TempFile::from_tree("containers-set", &with_photo());
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "containers-set");
    // Read back before and after the commit alike; member 2 holds it.
    assert_eq!(
        run(&mut client, 16),
        "0|a.txt|5|aGVsbG8=\r\n|0|a.txt|5|aGVsbG8=\r\n|0|3"
    );
    assert_eq!(member_state(&mut client, 2), (Some(1), vec![1, 2], Some(2)));
    let records = server::records_of(&mut client, TABLE);
    assert_eq!(server::cell(&records, 3, PHOTO), Some("02\u{e000}"));
    // The same data again: the member counts two.
    assert_eq!(run(&mut client, 21), "0");
    assert_eq!(member_state(&mut client, 2), (Some(2), vec![1, 2], Some(2)));

    // A decoded PNG named .png is stored as a picture with its size.
    assert_eq!(run(&mut client, 17), "0|red.png|3x2|119|?");
    assert_eq!(member_state(&mut client, 3), (Some(1), vec![1, 3], Some(3)));

    // A copy of record 1's value is one more reference to member 1.
    assert_eq!(run(&mut client, 18), "0|photo2.png|1");
    assert_eq!(member_state(&mut client, 1), (Some(2), vec![1, 3], Some(3)));

    // Text stays text: no member.
    assert_eq!(run(&mut client, 19), "0|hello|5|aGVsbG8=\r\n|0");
    assert_eq!(member_state(&mut client, 4).1, vec![1, 3]);

    // Clearing record 1 releases its reference.
    assert_eq!(run(&mut client, 20), "0||0||1");
    assert_eq!(member_state(&mut client, 1).0, Some(1));
    client.disconnect().unwrap();
}

#[test]
fn reverted_and_unsaved_container_data_stores_nothing() {
    let file = TempFile::from_tree("containers-revert", &with_photo());
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "containers-revert");
    assert_eq!(run(&mut client, 22), "0|2");
    assert_eq!(member_state(&mut client, 2), (None, vec![1, 1], Some(1)));
    assert_eq!(run(&mut client, 24), "0|2");
    assert_eq!(member_state(&mut client, 2), (None, vec![1, 1], Some(1)));

    // A commit that cannot be saved stores nothing.
    let blocked = BlockedSaves::new(&file);
    let reply = client.perform_script_on_server(16, "", true).unwrap();
    assert_eq!(reply.error, 800);
    assert_eq!(member_state(&mut client, 2), (None, vec![1, 1], Some(1)));
    drop(blocked);

    // Two records of a transaction share one member.
    assert_eq!(run(&mut client, 23), "0|4");
    assert_eq!(member_state(&mut client, 2), (Some(2), vec![1, 2], Some(2)));
    client.disconnect().unwrap();
}

/// Export Field Contents writes the container's data into the Documents
/// folder, through the folder's handle like any export.
#[test]
fn export_field_contents_writes_the_data() {
    let file = TempFile::from_tree("containers-export", &with_photo());
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "containers-export");
    let name = format!("om-photo-{}.png", std::process::id());
    let reply = client.perform_script_on_server(25, &name, true).unwrap();
    assert_eq!((reply.error, reply.result.as_str()), (0, "0"));
    let written = file.0.parent().unwrap().join("Documents").join(&name);
    let bytes = std::fs::read(&written).unwrap();
    let _ = std::fs::remove_file(&written);
    assert_eq!(bytes, png());
    // Outside the folders: 800, nothing written.
    let reply = client
        .perform_script_on_server(25, "../om-photo-outside.png", true)
        .unwrap();
    assert_eq!(reply.result, "800");
    client.disconnect().unwrap();
}

/// A loopback HTTP server answering every request with `body` and, when
/// given, a Content-Disposition header; returns its port.
fn serve(body: Vec<u8>, disposition: Option<&'static str>, requests: usize) -> u16 {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(requests) {
            let mut stream = stream.unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap() > 2 {
                line.clear();
            }
            let mut head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n",
                body.len()
            );
            if let Some(d) = disposition {
                head.push_str(&format!("Content-Disposition: {d}\r\n"));
            }
            head.push_str("\r\n");
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
        }
    });
    port
}

/// Insert from URL into a container field stores the response as
/// container data, typed by the URL's last path segment and named by
/// Content-Disposition, under a file path (s34-containers-http); a file:
/// URL reads only beneath the Documents folder; text fields take text.
#[test]
fn insert_from_url_stores_container_data() {
    let file = TempFile::from_tree("containers-url", &with_photo());
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "containers-url");
    let run_with = |client: &mut Session, script: u64, parameter: &str| {
        let reply = client
            .perform_script_on_server(script, parameter, true)
            .unwrap();
        reply.result
    };
    // PNGf 73 + 8, SIZE 4 + 8, FNAM (file path "a b.png") 18 + 8.
    let http = serve(png(), None, 1);
    assert_eq!(
        run_with(
            &mut client,
            26,
            &format!("http://127.0.0.1:{http}/x/a%20b.png?q=1")
        ),
        "0|a b.png|3x2|119|3"
    );
    let named = serve(png(), Some("attachment; filename=\"other.gif\""), 1);
    assert_eq!(
        run_with(
            &mut client,
            26,
            &format!("http://127.0.0.1:{named}/red.png")
        ),
        "0|other.gif|3x2|121|4"
    );
    // Beneath the Documents folder only.
    let documents = file.0.parent().unwrap().join("Documents");
    std::fs::create_dir_all(&documents).unwrap();
    let local = documents.join(format!("om-url-{}.png", std::process::id()));
    std::fs::write(&local, png()).unwrap();
    let result = run_with(&mut client, 26, &format!("file://{}", local.display()));
    let _ = std::fs::remove_file(&local);
    assert!(result.starts_with("0|om-url-"), "{result}");
    assert!(run_with(&mut client, 26, "file:///etc/hostname").starts_with("100|"));
    // A text field takes UTF-8 text; a picture there is 507.
    let text = serve("héllo".as_bytes().to_vec(), None, 1);
    assert_eq!(
        run_with(&mut client, 27, &format!("http://127.0.0.1:{text}/t.txt")),
        "0|héllo"
    );
    let binary = serve(png(), None, 1);
    assert_eq!(
        run_with(
            &mut client,
            27,
            &format!("http://127.0.0.1:{binary}/red.png")
        ),
        "507|"
    );
    client.disconnect().unwrap();
}

/// Tasks with Photo and [`fixture::PHOTO_CALCULATIONS`].
fn with_calculations() -> OwnedNode {
    let mut root = with_photo();
    fixture::add_photo_calculations(&mut root);
    root
}

/// Stored and auto-enter calculations see a container's data, not the
/// reference its cell holds (script oracle s34-containers-calc): decoded
/// data, text, and nothing once cleared.
#[test]
fn calculations_of_a_record_read_its_container_data() {
    let file = TempFile::from_tree("containers-calc", &with_calculations());
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "containers-calc");
    assert_eq!(
        run(&mut client, 28),
        "a.txt~5~aGVsbG8=\r\n~5|a.txt/~5~aGVsbG8=\r\n~5|hello/~~~"
    );
    client.disconnect().unwrap();
}

#[test]
fn server_unavailable_export_keeps_existing_destination() {
    let mut root = with_photo();
    fixture::add_unavailable_external_photo(&mut root);
    let file = TempFile::from_tree("container-server-export-unavailable", &root);
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "container-server-export-unavailable");
    let name = format!("container-existing-{}.bin", std::process::id());
    let destination = file.0.parent().unwrap().join("Documents").join(&name);
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::write(&destination, b"keep existing export").unwrap();
    let reply = client.perform_script_on_server(25, &name, true).unwrap();
    let bytes = std::fs::read(&destination).unwrap();
    let _ = std::fs::remove_file(&destination);
    client.disconnect().unwrap();
    assert_eq!(
        bytes, b"keep existing export",
        "unavailable external stream must refuse before truncating destination"
    );
    assert_eq!(reply.result, "1225");
}

#[test]
fn server_available_export_replaces_existing_destination() {
    let file = TempFile::from_tree("container-server-export-available", &with_photo());
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "container-server-export-available");
    let name = format!("container-available-{}.bin", std::process::id());
    let destination = file.0.parent().unwrap().join("Documents").join(&name);
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::write(&destination, b"old destination").unwrap();
    let reply = client.perform_script_on_server(25, &name, true).unwrap();
    let bytes = std::fs::read(&destination).unwrap();
    let _ = std::fs::remove_file(&destination);
    client.disconnect().unwrap();
    assert_eq!(bytes, png());
    assert_eq!((reply.error, reply.result.as_str()), (0, "0"));
}
