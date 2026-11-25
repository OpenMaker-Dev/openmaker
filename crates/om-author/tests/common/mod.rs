//! An `om-server` on loopback hosting the synthetic Tasks file
//! (om-server's `tests/fixtures/tasks.rs`) with the scripts the authoring
//! tests edit, and service targets for its accounts.
#![allow(dead_code)]

#[path = "../../../om-server/tests/fixtures/tasks.rs"]
pub mod fixture;

use std::path::{Path, PathBuf};
use std::time::Duration;

use om_author::Target;
use om_data::field_catalog;
use om_hbam::slot::uint_segment;
use om_hbam::{serial, OwnedNode};
use om_protocol::{LoginEnvironment, MachineInfo};
use om_provider::{NativeConfig, NativeProvider};
use om_scripts::Script;
use om_server::{HostedFile, Identity, Server, ServerConfig, ServerHandle, ServerIdentity};
use om_session::Session;
use serde_json::Value;
use zeroize::Zeroizing;

// Each test binary uses some of these.
#[allow(unused_imports)]
pub use fixture::{TempFile, ACCOUNT, CLERK, CLERK_PASSWORD, FILE, PASSWORD};

/// "Greeting": `Set Variable [ $who ; "world" ]`, `Exit Script [ "hello" ]`.
/// Left out of the Scripts menu, run with full access privileges, with an
/// item UUID on its first step, a key FileMaker does not define here, and
/// step data no step uses: all kept by every step edit.
pub const GREETING: u64 = 5;
/// Two scripts named "Twin".
pub const TWINS: [u64; 2] = [6, 7];
pub const UNKNOWN_KEY: u8 = 0x77;

pub fn greeting() -> Script {
    let mut script = fixture::script(
        GREETING,
        "Greeting",
        vec![
            fixture::set_variable("$who", "\"world\""),
            fixture::exit_with("\"hello\""),
        ],
    );
    script.set_in_scripts_menu(false);
    script.set_runs_with_full_access(true);
    script.steps[0].set_unique_ids([5; 16], None);
    script
        .metadata
        .push(OwnedNode::leaf(vec![UNKNOWN_KEY], vec![1, 2, 3]));
    script.unreferenced_data.push(OwnedNode::list(
        serial::encode(40),
        vec![OwnedNode::leaf(vec![0xd8], vec![9; 16])],
    ));
    script
}

pub fn scripts() -> Vec<Script> {
    let mut scripts = vec![fixture::server_work(), greeting()];
    for id in TWINS {
        scripts.push(fixture::script(id, "Twin", vec![fixture::exit_with("1")]));
    }
    scripts
}

/// A key FileMaker does not define in a field catalog's index, which every
/// field edit keeps.
pub const UNKNOWN_INDEX_KEY: u8 = 0x77;

fn key_string(key: &[u8]) -> Vec<u8> {
    let mut out = vec![key.len() as u8];
    out.extend(key);
    out
}

/// The Tasks file with [`scripts`], each table's field catalog indexed as
/// FileMaker indexes it (schema-path.md 5.1, 5.5): the last issued id, the
/// name list, the name list by type, the display order, the count and a
/// catalog UUID, and [`UNKNOWN_INDEX_KEY`].
pub fn indexed_tree() -> OwnedNode {
    let mut root = fixture::tree(&scripts(), true);
    for table in [fixture::TABLE, fixture::NOTES] {
        let t = uint_segment(table);
        let catalog = root.at(&[&t, &[3]]).cloned().expect("a field catalog");
        let mut index = OwnedNode::list(vec![1], Vec::new());
        let mut last = 0;
        for (n, member) in field_catalog::members(&catalog).iter().enumerate() {
            last = last.max(serial::decode(&member.segment).expect("a serial key"));
            let name = field_catalog::member_name(member).expect("a name");
            let definition = field_catalog::member_definition(member).expect("a definition");
            let key = field_catalog::name_key(&name);
            let id = key_string(&member.segment);
            let mut listed = key.clone();
            listed.extend([0, 0, 0]);
            let mut typed = vec![definition[0], definition[1] << 2];
            typed.extend(&key);
            typed.extend([0, 0]);
            index.put(&[&[1], &listed], OwnedNode::leaf(Vec::new(), id.clone()));
            index.put(&[&[6], &typed], OwnedNode::leaf(Vec::new(), id.clone()));
            index.put(
                &[&[3], &(n as u32 + 1).to_be_bytes()],
                OwnedNode::leaf(Vec::new(), id),
            );
        }
        let count = field_catalog::members(&catalog).len() as u32;
        index.set_child(OwnedNode::leaf(vec![0], key_string(&serial::encode(last))));
        index.set_child(OwnedNode::leaf(vec![4], count.to_be_bytes().to_vec()));
        index.set_child(OwnedNode::leaf(vec![0xd8], vec![7; 16]));
        index.set_child(OwnedNode::leaf(vec![UNKNOWN_INDEX_KEY], vec![1, 2, 3]));
        root.put(&[&t, &[3], &[1]], index);
    }
    root
}

/// A key FileMaker does not define in a layout member, which every
/// placement keeps.
pub const UNKNOWN_LAYOUT_KEY: u8 = 0x77;
/// The Form layout's objects in [`form_tree`]: a label, the Name field and
/// a rectangle along the body's foot.
pub const NAME_LABEL: u64 = 1;
pub const NAME_FIELD: u64 = 2;
pub const FOOT_RECTANGLE: u64 = 3;

fn bounds(left: f64, top: f64, width: f64, height: f64) -> om_layout::Rect {
    om_layout::Rect {
        top,
        left,
        bottom: top + height,
        right: left + width,
        other: Vec::new(),
    }
}

/// [`indexed_tree`] with its Tasks layout ([`fixture::LAYOUT`]) a simple
/// Form layout as FileMaker Pro stores one (layout-model.md 4, 5): 480
/// points wide, a header (0 to 40) and a body (40 to 240); in the body a
/// label "Name" ([`NAME_LABEL`]), the Name field ([`NAME_FIELD`]) and a
/// rectangle ([`FOOT_RECTANGLE`]), each with its attribute record and
/// UUIDs; each part's list of its objects; the field reference index; the
/// layout's stamp; and [`UNKNOWN_LAYOUT_KEY`].
pub fn form_tree() -> OwnedNode {
    use om_layout::encode::{self, bytes, double, uint};
    let mut root = indexed_tree();
    let layout = uint_segment(fixture::LAYOUT);
    let occurrence = serial::decode(&uint_segment(fixture::OCCURRENCE)).unwrap();
    let mut member = root
        .at(&[&[4], &[5], &layout])
        .cloned()
        .expect("the Tasks layout");
    let mut def = Vec::new();
    uint(2, occurrence, &mut def);
    double(8, 480.0, &mut def);
    member.set_child(OwnedNode::leaf(vec![2], def));
    for (part, kind, top, height) in [(1u8, 1u64, 0.0, 40.0), (2, 4, 40.0, 200.0)] {
        let mut definition = Vec::new();
        uint(1, kind, &mut definition);
        double(4, top, &mut definition);
        double(5, height, &mut definition);
        member.put(
            &[&[3], &[part], &[2]],
            OwnedNode::leaf(Vec::new(), definition),
        );
        // Each part's list of its objects (layout-model.md 10.5): the
        // header holds none, the body the label, the field and the
        // rectangle.
        let mut list = vec![0, 0, 0, 0];
        if kind == 4 {
            list[3] = 3;
            for key in [NAME_LABEL, NAME_FIELD, FOOT_RECTANGLE] {
                let key = object_key_string(key);
                list.push(key.len() as u8);
                list.extend(key);
            }
        }
        member.put(&[&[3], &[part], &[7]], OwnedNode::leaf(Vec::new(), list));
    }
    let mut rectangle = Vec::new();
    uint(1, 5, &mut rectangle);
    uint(2, FOOT_RECTANGLE, &mut rectangle);
    uint(3, encode::ANCHOR_LEFT_TOP, &mut rectangle);
    bytes(
        4,
        &encode::rect(&bounds(0.0, 200.0, 480.0, 40.0)),
        &mut rectangle,
    );
    bytes(5, &encode::default_style(51), &mut rectangle);
    let mut store = Vec::new();
    for key in [NAME_LABEL, NAME_FIELD, FOOT_RECTANGLE] {
        uint(1, key, &mut store);
    }
    let objects = [
        encode::text_object(NAME_LABEL, &bounds(20.0, 50.0, 80.0, 20.0)),
        encode::field_object(
            NAME_FIELD,
            &bounds(110.0, 50.0, 200.0, 20.0),
            occurrence,
            fixture::NAME,
        ),
        rectangle,
    ];
    for object in &objects {
        bytes(2, object, &mut store);
    }
    member.set_child(OwnedNode::leaf(vec![7], store));
    for (key, uuid) in [
        (NAME_LABEL, 0x11u8),
        (NAME_FIELD, 0x22),
        (FOOT_RECTANGLE, 0x33),
    ] {
        let record = serial::encode(key);
        let at = |child: u8| [vec![5u8], record.clone(), vec![child]];
        let put = |member: &mut OwnedNode, child: u8, node: OwnedNode| {
            let path = at(child);
            let path: Vec<&[u8]> = path.iter().map(Vec::as_slice).collect();
            member.put(&path, node);
        };
        put(&mut member, 0x10, OwnedNode::leaf(Vec::new(), Vec::new()));
        put(
            &mut member,
            0xd8,
            OwnedNode::leaf(Vec::new(), vec![uuid; 16]),
        );
        put(
            &mut member,
            0xd9,
            OwnedNode::leaf(Vec::new(), vec![uuid + 1; 16]),
        );
        if key == NAME_LABEL {
            put(&mut member, 1, om_scripts::text_node(&[], "Name"));
        }
    }
    member.put(
        &[
            &[1],
            &uint_segment(fixture::OCCURRENCE),
            &uint_segment(fixture::NAME),
            &serial::encode(NAME_FIELD),
        ],
        OwnedNode::leaf(Vec::new(), Vec::new()),
    );
    // The object index entry of the Name field (at top 50, left 110, the
    // second object in pre-order) and the last object key issued, as
    // FileMaker keeps them (layout-model.md 10.5).
    let mut entry = vec![0, 2];
    entry.extend(object_key_string(NAME_FIELD));
    entry.resize(5, 0);
    entry.extend([0, 0, 0, 1]);
    member.put(
        &[&[0x0a], &[0, 50, 0, 110, 0, 1]],
        OwnedNode::leaf(Vec::new(), entry),
    );
    member.put(
        &[&[5], &[0]],
        OwnedNode::leaf(Vec::new(), object_key_string(FOOT_RECTANGLE)),
    );
    member.set_child(OwnedNode::leaf(vec![0xfc], vec![1, 3]));
    member.set_child(om_scripts::text_node(&[0xfc, 0x01], "Designer"));
    member.set_child(OwnedNode::leaf(vec![UNKNOWN_LAYOUT_KEY], vec![4, 5, 6]));
    root.put(&[&[4], &[5], &layout], member);
    root
}

/// An object key as a key string: its length, then the serial key.
pub fn object_key_string(key: u64) -> Vec<u8> {
    let encoded = serial::encode(key);
    let mut out = vec![encoded.len() as u8];
    out.extend(encoded);
    out
}

/// A running server and the CA its clients trust.
pub struct Host {
    pub port: u16,
    pub ca_pem: Vec<u8>,
    pub handle: ServerHandle,
}

impl Host {
    pub fn stop(&self) {
        assert!(self.handle.shutdown(Duration::from_secs(10)));
    }
}

/// Host `file`, reporting `version_string` in the Connect reply. Admin
/// has [Full Access]; Clerk is in [Data Entry Only] (the file's own
/// accounts, `fixtures/tasks.rs`).
pub fn host(file: &TempFile, version_string: &str) -> Host {
    host_configured(file, version_string, |server| server)
}

/// [`host`] for a file FileMaker Server would not host, such as a starter
/// solution, whose Admin has [Full Access] and an empty password
/// (`ServerConfig::insecure_files`).
pub fn host_insecure(file: &TempFile, version_string: &str) -> Host {
    serve(file, version_string, true, None, |server| server)
}

/// [`host`] with recording enabled to check what reaches the wire.
pub fn host_recording(file: &TempFile, record_dir: &Path) -> Host {
    serve(
        file,
        "OpenMaker Server test",
        false,
        Some(record_dir.to_path_buf()),
        |server| server,
    )
}

/// [`host`], the server configured by `configure` first.
pub fn host_configured(
    file: &TempFile,
    version_string: &str,
    configure: impl FnOnce(Server) -> Server,
) -> Host {
    serve(file, version_string, false, None, configure)
}

fn serve(
    file: &TempFile,
    version_string: &str,
    insecure_files: bool,
    record_dir: Option<PathBuf>,
    configure: impl FnOnce(Server) -> Server,
) -> Host {
    let generated = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let ca_pem = generated.cert.pem().into_bytes();
    let identity =
        Identity::from_pem(&ca_pem, generated.signing_key.serialize_pem().as_bytes()).unwrap();
    let config = ServerConfig {
        identity: ServerIdentity {
            name: "openmaker-test".into(),
            version_string: version_string.into(),
            server_id: "0123456789abcdef0123456789abcdef".into(),
            web_service_settings: String::new(),
        },
        files: vec![HostedFile {
            name: FILE.into(),
            path: file.0.clone(),
        }],
        insecure_files,
        record_dir,
    };
    let server = configure(Server::bind("127.0.0.1:0", config, identity).unwrap());
    let port = server.local_addr().unwrap().port();
    let handle = server.handle().unwrap();
    std::thread::spawn(move || server.serve());
    Host {
        port,
        ca_pem,
        handle,
    }
}

pub fn tls(host: &Host) -> NativeConfig {
    let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
    NativeConfig {
        client_cert_pem: client.cert.pem().into_bytes(),
        client_key_pem: client.signing_key.serialize_pem().into_bytes(),
        additional_ca_pem: host.ca_pem.clone(),
        callback_host: "127.0.0.1".into(),
        machine: MachineInfo::default(),
    }
}

pub fn target(host: &Host, account: &str, password: &str) -> Target {
    Target::new(
        "127.0.0.1",
        host.port,
        FILE,
        account,
        Zeroizing::new(password.into()),
        tls(host),
    )
}

/// The file opened by the desktop client's provider, as Admin.
pub fn provider(host: &Host) -> NativeProvider {
    use om_data::DataProvider;
    let mut provider = NativeProvider::new(tls(host));
    provider
        .open(&om_data::Connection {
            host: "127.0.0.1".into(),
            port: host.port,
            file: FILE.into(),
            account: ACCOUNT.into(),
            password: PASSWORD.into(),
        })
        .unwrap();
    provider
}

/// A protocol session logged in as `account`, as another client.
pub fn session(host: &Host, name: &str, account: &str, password: &str) -> (Session, u32) {
    let client = rcgen::generate_simple_self_signed(vec!["client".to_string()]).unwrap();
    let mut session = Session::connect(
        "127.0.0.1",
        host.port,
        client.cert.pem().as_bytes(),
        client.signing_key.serialize_pem().as_bytes(),
        "127.0.0.1",
        &MachineInfo {
            machine_name: name.into(),
            ..MachineInfo::default()
        },
    )
    .unwrap();
    let open = session.open_file("127.0.0.1", FILE).unwrap();
    let creation = session.fetch_internal_creation().unwrap();
    let login = session
        .login(
            account,
            password,
            creation,
            &LoginEnvironment::for_remote_file("127.0.0.1", FILE),
        )
        .unwrap();
    assert!(login.is_success(), "{login:?}");
    (session, open.file_id)
}

/// The result of a response that must have succeeded.
pub fn ok(response: Value) -> Value {
    assert_eq!(response["ok"], true, "{response:#}");
    response["result"].clone()
}

/// The error of a response that must have failed.
pub fn error(response: Value) -> Value {
    assert_eq!(response["ok"], false, "{response:#}");
    response["error"].clone()
}

/// A revision as text (`sha256:…`) as bytes.
pub fn revision(text: &str) -> [u8; 32] {
    let hex = text.strip_prefix("sha256:").expect("a revision");
    let mut out = [0; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap();
    }
    out
}

pub fn text(value: &Value) -> &str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("not text: {value}"))
}
