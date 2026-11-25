//! Root-file privilege transport against a synthetic, in-process server.
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
#[path = "common/related.rs"]
mod related;
#[path = "common/privileges.rs"]
mod security;

use om_data::calc::eval::{evaluate_text, Context, Variables};
use om_data::calc::{Ctx, Date};
use om_data::host::{FileCalc, FileHost, MemorySource, SessionInfo};
use om_data::{Connection, DataError, DataProvider};
use om_protocol::MachineInfo;
use om_provider::{NativeConfig, NativeProvider};
use om_server::{HostedFile, Identity, Server, ServerConfig, ServerIdentity};

fn open(root: &om_hbam::OwnedNode, account: &str) -> (fixture::TempFile, NativeProvider) {
    let (file, provider, opened) = connect(root, account);
    opened.unwrap();
    (file, provider)
}

fn connect(
    root: &om_hbam::OwnedNode,
    account: &str,
) -> (fixture::TempFile, NativeProvider, Result<(), DataError>) {
    let file = fixture::TempFile::from_tree("desktop-privileges", root);
    let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
    let ca = cert.cert.pem().into_bytes();
    let config = ServerConfig {
        identity: ServerIdentity {
            name: "synthetic".into(),
            version_string: "OpenMaker Server test".into(),
            server_id: "0123456789abcdef0123456789abcdef".into(),
            web_service_settings: String::new(),
        },
        files: vec![HostedFile {
            name: fixture::FILE.into(),
            path: file.0.clone(),
        }],
        insecure_files: false,
        record_dir: None,
    };
    let identity = Identity::from_pem(&ca, cert.key_pair.serialize_pem().as_bytes()).unwrap();
    // Accounts in custom sets too: these tests are about what the client
    // does with an account's privilege metadata (#734).
    let server = Server::bind("127.0.0.1:0", config, identity)
        .unwrap()
        .admitting_custom_sets();
    let port = server.local_addr().unwrap().port();
    std::thread::spawn(move || server.serve());
    let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
    let mut provider = NativeProvider::new(NativeConfig {
        client_cert_pem: client.cert.pem().into_bytes(),
        client_key_pem: client.key_pair.serialize_pem().into_bytes(),
        additional_ca_pem: ca,
        callback_host: "127.0.0.1".into(),
        machine: MachineInfo::default(),
    });
    let opened = provider
        .open(&Connection {
            host: "127.0.0.1".into(),
            port,
            file: fixture::FILE.into(),
            account: account.into(),
            password: fixture::PASSWORD.into(),
        })
        .map(|_| ());
    (file, provider, opened)
}

fn answers(provider: &mut NativeProvider) -> String {
    let schema = provider.calc_schema().unwrap().unwrap();
    let privileges = provider.account_privileges();
    let session = SessionInfo {
        privilege_set_name: privileges
            .as_ref()
            .map(|p| p.account_set_name.clone())
            .unwrap_or_default(),
        privileges,
        ..Default::default()
    };
    let source = MemorySource::new();
    let today = Date::from_ymd(2026, 10, 31).unwrap();
    let (mut locals, mut globals) = (Variables::new(), Variables::new());
    let mut host = FileHost::new(
        FileCalc::new(&schema, &source, today),
        &session,
        &mut locals,
        &mut globals,
    );
    let result = evaluate_text("Get ( CurrentPrivilegeSetName ) & \"|\" & Get ( AccountPrivilegeSetName ) & \"|\" & Substitute ( Get ( CurrentExtendedPrivileges ) ; Char ( 13 ) ; \",\" ) & \"|\" & Substitute ( Get ( AccountExtendedPrivileges ) ; Char ( 13 ) ; \",\" ) & \"|\" & Get ( LayoutAccess ) & \"|\" & Get ( RecordAccess )", &mut host, &Context::new());
    assert_eq!(result.code(), 0);
    result
        .value
        .to_text(&Ctx::new(&schema.locale, today))
        .into_string()
}

#[test]
fn desktop_catalog_answers_full_custom_read_only_and_entry_sets() {
    for (key, name, extended, records) in [
        (&[1][..], "[Full Access]", "fmapp,full-only", 2),
        (&[2][..], "[Data Entry Only]", "fmapp", 2),
        (&[3][..], "[Read-Only Access]", "fmapp", 1),
        (security::CUSTOM_KEY, "Office", "zeta,alpha", 2),
    ] {
        let mut root = security::tree(&[], key);
        fixture::add_field(
            &mut root,
            fixture::TABLE,
            90,
            "SetName",
            2,
            1,
            0,
            0x2_0000,
            0,
        );
        let mut names = fixture::names();
        names.add_field(fixture::TABLE, 90, "SetName");
        fixture::add_formula(
            &mut root,
            &names,
            (fixture::TABLE, fixture::OCCURRENCE),
            90,
            5,
            "Get ( AccountPrivilegeSetName )",
        );
        let (_file, mut provider) = open(&root, fixture::ACCOUNT);
        assert_eq!(
            answers(&mut provider),
            format!("{name}|{name}|{extended}|{extended}||{records}")
        );
        let page = provider.records(fixture::LAYOUT as u32, 0, 10).unwrap();
        let field = page
            .fields
            .iter()
            .position(|f| f.name == "SetName")
            .unwrap();
        assert_eq!(page.records[0].values[field].as_deref(), Some(name));
        assert_eq!(
            provider.full_access_privileges().unwrap().extended,
            ["fmapp", "full-only"]
        );
        // Security data must not appear in the calculation schema or data.
        let schema = format!("{:?}", provider.calc_schema().unwrap());
        assert!(!schema.contains(std::str::from_utf8(security::AUTH_SENTINEL).unwrap()));
        provider.close();
        assert!(provider.account_privileges().is_none());
        assert!(provider.full_access_privileges().is_none());
    }
}

/// Hosted Root uses its own authenticated account for Browse availability
/// as well as its write guard, independently of related-file hydration.
#[test]
fn hosted_read_only_root_offers_no_edits_and_preserves_the_owner_value() {
    let host = related::Host::new(
        "read-only-root",
        vec![(fixture::FILE, security::tree(&[], &[3]))],
    );
    let mut root = host.open(fixture::FILE);
    let mut observer = host.open(fixture::FILE);
    let layout = fixture::LAYOUT as u32;
    let before = observer.records(layout, 0, 1).unwrap();
    let count = before
        .fields
        .iter()
        .position(|f| f.name == "Count")
        .unwrap();
    let record = before.records[0].id;
    let stored = before.records[0].values[count].clone();
    let capabilities = root.record_capabilities(layout).unwrap();
    let denied = root
        .update_record(layout, record, [("Count".into(), Some("99".into()))].into())
        .unwrap_err();
    let after = observer.records(layout, 0, 1).unwrap();
    assert_eq!(after.records[0].values[count], stored);
    assert!(
        !capabilities.update,
        "Read-Only Root offers edits: {capabilities:?}"
    );
    assert!(!capabilities.can_edit(&before.fields[count]));
    assert!(!capabilities.create && !capabilities.delete);
    assert_eq!(denied.code, Some(200), "{denied:?}");
}

#[test]
fn missing_account_reference_never_borrows_another_accounts_set() {
    let known = security::tree(&[], security::CUSTOM_KEY);
    let (_known_file, known_provider) = open(&known, fixture::ACCOUNT);
    assert_eq!(
        known_provider
            .account_privileges()
            .unwrap()
            .account_set_name,
        "Office"
    );
    let mut root = fixture::tree(&[], true);
    assert!(root.remove(&[&[23], &[1], &[5], &[2], &[0x0b]]).is_some());
    let (_file, provider, opened) = connect(&root, fixture::ACCOUNT);
    // Authentication cannot establish password expiry without the account
    // reference. Refuse before exposing another account's set metadata.
    assert_eq!(opened.unwrap_err().code, Some(212));
    assert!(provider.account_privileges().is_none());
    assert!(provider.full_access_privileges().is_none());
}

#[test]
fn an_account_in_a_set_without_a_readable_name_cannot_log_in() {
    // The host cannot tell which set it is, so it cannot hold the account
    // to it: the login is refused (9, which a hosted login reports as a
    // refusal) before any metadata is read.
    let mut root = fixture::tree(&[], true);
    assert!(root.remove(&[&[23], &[2], &[5], &[1], &[0x10]]).is_some());
    let (_file, provider, opened) = connect(&root, fixture::ACCOUNT);
    assert_eq!(opened.unwrap_err().code, Some(212));
    assert!(provider.account_privileges().is_none());
}

#[test]
fn login_selects_opaque_multibyte_account_key_not_account_number() {
    let mut root = security::tree(&[], security::CUSTOM_KEY);
    let account = root.remove(&[&[23], &[1], &[5], &[2]]).unwrap();
    root.put(&[&[23], &[1], &[5], &[0x80, 0x81]], account);
    let (_file, mut provider) = open(&root, fixture::ACCOUNT);
    assert_eq!(
        provider.account_privileges().unwrap().account_set_name,
        "Office"
    );
    assert_eq!(
        answers(&mut provider),
        "Office|Office|zeta,alpha|zeta,alpha||2"
    );
}

/// `root` as a local file, open as Admin through the file's own host (the
/// desktop holds a local file's account to its set).
fn open_local(root: &om_hbam::OwnedNode, tag: &str) -> (fixture::TempFile, NativeProvider) {
    let file = fixture::TempFile::from_tree(tag, root);
    let provider = open_path(&file.0);
    (file, provider)
}

/// The local file at `path`, open as Admin through its own host.
fn open_path(path: &std::path::Path) -> NativeProvider {
    let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
    let mut provider = NativeProvider::new(NativeConfig {
        client_cert_pem: client.cert.pem().into_bytes(),
        client_key_pem: client.key_pair.serialize_pem().into_bytes(),
        additional_ca_pem: Vec::new(),
        callback_host: "127.0.0.1".into(),
        machine: MachineInfo::default(),
    });
    provider
        .open_local(&om_data::LocalFile {
            path: path.to_owned(),
            credentials: Some((fixture::ACCOUNT.into(), fixture::PASSWORD.to_owned().into())),
        })
        .unwrap();
    provider
}

/// A table the account's set may never view (Notes, under "Office"), in a
/// local file (the desktop holds a local file's account to its set): the
/// file's host withholds the records' values, and the provider marks them
/// in the calculation data, so that a portal row shows `<No Access>`
/// rather than blanks (#695). The layout's own table, which it may view,
/// is not marked.
#[test]
fn records_the_set_may_never_view_are_marked_in_calculation_data() {
    use om_data::host::CalcNeed;
    let mut root = security::tree(&[], security::CUSTOM_KEY);
    root.put(
        &[&[23], &[1], &[5], &[2], &[0x0a]],
        om_hbam::OwnedNode::leaf([], fixture::ACTIVE_STATUS.to_vec()),
    );
    root.put(
        &[
            &[23],
            &[2],
            &[5],
            security::CUSTOM_KEY,
            &[5],
            &om_hbam::slot::uint_segment(fixture::NOTES),
            &[2],
        ],
        om_hbam::OwnedNode::leaf([], vec![0x00, 0x03]),
    );
    let (_file, mut provider) = open_local(&root, "portal-no-access");
    let data = provider
        .calc_data(&[
            CalcNeed::Record {
                table: fixture::NOTES,
                record: 1,
            },
            CalcNeed::Record {
                table: fixture::TABLE,
                record: 1,
            },
        ])
        .unwrap();
    let note = data.records[&(fixture::NOTES, 1)].as_deref().unwrap();
    assert!(note.no_access && note.values.is_empty(), "{note:?}");
    let task = data.records[&(fixture::TABLE, 1)].as_deref().unwrap();
    assert!(!task.no_access && !task.values.is_empty(), "{task:?}");
}

/// The Tasks file with Admin in the custom set "Office", which may create
/// and delete Notes records, never edit them, and view them `view`: never
/// (`None`), or where formula `Some(text)` is true.
fn notes_limited(view: Option<&str>) -> om_hbam::OwnedNode {
    let mut root = security::tree(&[], security::CUSTOM_KEY);
    root.put(
        &[&[23], &[1], &[5], &[2], &[0x0a]],
        om_hbam::OwnedNode::leaf([], fixture::ACTIVE_STATUS.to_vec()),
    );
    let notes = om_hbam::slot::uint_segment(fixture::NOTES);
    let entry: [&[u8]; 6] = [&[23], &[2], &[5], security::CUSTOM_KEY, &[5], &notes];
    // Create, view, edit and delete, two bits each: yes, no or limited
    // (2), no, yes.
    let actions = if view.is_some() { 0x61 } else { 0x41 };
    root.put(
        &[&entry[..], &[&[2][..]]].concat(),
        om_hbam::OwnedNode::leaf([], vec![actions, 0x03]),
    );
    if let Some(text) = view {
        let code = fixture::calc_in(fixture::NOTES_OCCURRENCE, text)
            .polish
            .unwrap();
        root.put(
            &[&entry[..], &[&[7][..], &[6][..]]].concat(),
            om_hbam::OwnedNode::leaf([], code),
        );
    }
    root
}

/// Notes record `record`'s Body as the file on disk holds it.
fn body_on_disk(path: &std::path::Path, record: i64) -> Option<String> {
    let file = om_hbam::HbamFile::open(path).unwrap();
    let tree = file.key_tree().unwrap();
    let notes = om_hbam::slot::uint_segment(fixture::NOTES);
    let records = tree.node(&[&notes, &[5]])?;
    let body = om_hbam::slot::uint_segment(fixture::BODY);
    records
        .children
        .iter()
        .find(|r| om_hbam::serial::decode(r.segment.0) == u64::try_from(record).ok())?
        .child(&body)?
        .assembled()
        .and_then(|bytes| om_scsu::decode_scsu(&bytes).ok())
}

/// A new record the account's set may not view is created: the host
/// serves it back as its id and modification count, without its values,
/// and the provider takes that for what it is, not for a lost write
/// ("saving it could not be verified"). FileMaker Pro 26.0.3's New Record
/// creates such a record under #137's `limited` set, and Delete Record
/// removes it. The file on disk holds what was written. Where the view
/// formula is true on an empty record, an empty read-back could also be a
/// record the host kept empty: the create is reported as unverified, as
/// before.
#[test]
fn a_new_record_the_set_may_not_view_is_created_and_deleted() {
    use om_data::host::CalcNeed;
    let typed = || om_data::FieldChanges::from([("Body".into(), Some("Hidden".into()))]);
    for (view, tag) in [
        (None, "create-never-viewed"),
        (Some("Body = \"Hi\""), "create-view-formula"),
    ] {
        let (file, mut provider) = open_local(&notes_limited(view), tag);
        if view.is_some() {
            // The formula is judged: the fixture's note "Hi" is viewable.
            let data = provider
                .calc_data(&[CalcNeed::Record {
                    table: fixture::NOTES,
                    record: 1,
                }])
                .unwrap();
            let note = data.records[&(fixture::NOTES, 1)].as_deref().unwrap();
            assert!(!note.no_access, "{tag}: {note:?}");
        }
        let created = provider
            .create_record(fixture::NOTES_LAYOUT as u32, typed())
            .unwrap();
        assert!(
            created.values.iter().all(Option::is_none),
            "{tag}: {created:?}"
        );
        let id = u64::try_from(created.id).unwrap();
        let data = provider
            .calc_data(&[CalcNeed::Record {
                table: fixture::NOTES,
                record: id,
            }])
            .unwrap();
        let note = data.records[&(fixture::NOTES, id)].as_deref().unwrap();
        assert!(note.no_access && note.values.is_empty(), "{tag}: {note:?}");
        provider.close();
        assert_eq!(
            body_on_disk(&file.0, created.id).as_deref(),
            Some("Hidden"),
            "{tag}"
        );
        let mut provider = open_path(&file.0);
        provider
            .delete_related_record(fixture::NOTES, created.id)
            .unwrap();
    }
    let (file, mut provider) = open_local(
        &notes_limited(Some("IsEmpty ( Body )")),
        "create-empty-viewable",
    );
    let refused = provider
        .create_record(fixture::NOTES_LAYOUT as u32, typed())
        .unwrap_err();
    assert!(
        refused.message.contains("could not be verified"),
        "{refused:?}"
    );
    provider.close();
    // Record 2, after the fixture's note 1: the host kept it.
    assert_eq!(body_on_disk(&file.0, 2).as_deref(), Some("Hidden"));
}

/// A layout the account's set shows with its records without access
/// (`d7`), as FileMaker Pro 26.0.3 measured it on #707: the account is
/// admitted (it was refused with 9); through the layout `Get (
/// RecordAccess )` keeps the table's answer (2) and a user's edit is
/// refused (200), while a script's write goes through and is saved.
#[test]
fn a_layout_without_record_access_admits_the_account_and_lets_scripts_write() {
    let mut root = security::tree(&[], security::CUSTOM_KEY);
    root.put(
        &[&[23], &[1], &[5], &[2], &[0x0a]],
        om_hbam::OwnedNode::leaf([], fixture::ACTIVE_STATUS.to_vec()),
    );
    // Office: the Tasks layout shown, its records without access.
    root.put(
        &[
            &[23],
            &[2],
            &[5],
            security::CUSTOM_KEY,
            &[9],
            &[0x04, fixture::LAYOUT as u8],
        ],
        om_hbam::OwnedNode::leaf([], vec![0xd7]),
    );
    let (_file, mut provider) = open_local(&root, "layout-records");
    let layout = fixture::LAYOUT as u32;
    assert_eq!(provider.record_access(layout, 1), Some(2));
    // Every field of every record shows `<No Access>` through the layout.
    let page = provider.records(layout, 0, 10).unwrap();
    let hidden = provider.no_access(layout, &page);
    assert!(hidden.layout && hidden.hides(1, "Name"));
    assert_eq!(hidden.record_access(1), Some(2));
    let typed = om_data::FieldChanges::from([("Name".into(), Some("Typed".into()))]);
    assert_eq!(
        provider.update_record(layout, 1, typed).unwrap_err().code,
        Some(200)
    );
    let write = om_data::ScriptWrite {
        layout,
        record: Some(1),
        cells: std::collections::BTreeMap::from([(fixture::NAME, Some("Scripted".into()))]),
        ..om_data::ScriptWrite::default()
    };
    provider.write_record(&write).unwrap();
    let page = provider.records(layout, 0, 10).unwrap();
    let name = page.fields.iter().position(|f| f.name == "Name").unwrap();
    let record = page.records.iter().find(|r| r.id == 1).unwrap();
    assert_eq!(record.values[name].as_deref(), Some("Scripted"));
}

/// A layout the account's set hides gives its found set's ids and count,
/// and takes a found-set change that follows no value, but no value: a
/// script's New Window or Go to Related Record takes a window there, as
/// FileMaker Pro 26.0.3 does (#696). Its layout is still refused, and so
/// are a find and a sort through it.
#[test]
fn a_hidden_layout_gives_its_found_set_and_no_values() {
    let mut root = security::tree(&[], security::CUSTOM_KEY);
    root.put(
        &[&[23], &[1], &[5], &[2], &[0x0a]],
        om_hbam::OwnedNode::leaf([], fixture::ACTIVE_STATUS.to_vec()),
    );
    // Office: the Notes layout hidden.
    root.put(
        &[
            &[23],
            &[2],
            &[5],
            security::CUSTOM_KEY,
            &[9],
            &[0x04, fixture::NOTES_LAYOUT as u8],
        ],
        om_hbam::OwnedNode::leaf([], vec![0xd5]),
    );
    let (_file, mut provider) = open_local(&root, "hidden-found-set");
    let notes = fixture::NOTES_LAYOUT as u32;
    assert_eq!(provider.render_layout(notes).unwrap_err().code, Some(9));
    let page = provider.records(notes, 0, 10).unwrap();
    assert_eq!((page.total, page.records.len()), (1, 1));
    assert!(page.fields.is_empty() && page.records[0].values.is_empty());
    let found = provider
        .change_found_set(notes, om_data::found::FoundSetChange::ShowAll)
        .unwrap();
    assert_eq!((found.found, found.total), (1, 1));
    // A find or sort through it would tell its values by what it finds or
    // how it orders them: refused (3).
    let refused = provider
        .change_found_set(notes, om_data::found::FoundSetChange::QuickFind(Vec::new()))
        .unwrap_err();
    assert_eq!(refused.code, Some(3));
}
