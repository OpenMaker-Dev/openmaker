//! OpenMaker's client without a FileMaker license: opening `.fmp12`
//! files from this computer itself (`open_local`, through the private
//! `om_server::LocalHost`), and connecting to OpenMaker Server with an
//! empty license (install code). The file is the synthetic Tasks file of
//! `om-server/tests/fixtures/tasks.rs`, with local accounts (Admin in
//! [Full Access], Clerk in [Data Entry Only]). No license is set or read
//! anywhere in this file.

#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
#[path = "common/related.rs"]
mod related;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use fixture::{ACCOUNT, CLERK, CLERK_PASSWORD, FILE, NEWCOMER, NEWCOMER_PASSWORD, PASSWORD, TABLE};
use om_data::runtime::TransactionOptions;
use om_data::{Connection, DataProvider, FieldChanges, LocalFile, LocalOpen, RecordPage};
use om_protocol::MachineInfo;
use om_provider::{NativeConfig, NativeProvider};
use om_server::{HostedFile, Identity, Server, ServerConfig, ServerIdentity};

/// A folder of its own for each test, removed when dropped.
struct Folder(PathBuf);

impl Folder {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("om-local-files-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// The Tasks file with local accounts; `admin_password` replaces
    /// Admin's.
    fn tasks(&self, admin_password: Option<&str>) -> PathBuf {
        let mut root = fixture::tree(&[], true);
        fixture::add_local_accounts(&mut root);
        if let Some(password) = admin_password {
            root.put(
                &[&[23], &[1], &[5], &[2], &[6]],
                om_hbam::OwnedNode::leaf(
                    Vec::new(),
                    om_crypto::account::StoredPassword::encode(password, [7; 10]),
                ),
            );
        }
        let path = self.0.join(FILE);
        fixture::write_tree(&path, &root);
        path
    }

    /// A Tasks file with local accounts named `name` (with its extension),
    /// whose Admin password is `password`, changed by `change`.
    fn file(
        &self,
        name: &str,
        password: &str,
        change: impl FnOnce(&mut om_hbam::OwnedNode),
    ) -> PathBuf {
        let mut root = fixture::tree(&[], true);
        fixture::add_local_accounts(&mut root);
        root.put(
            &[&[23], &[1], &[5], &[2], &[6]],
            om_hbam::OwnedNode::leaf(
                Vec::new(),
                om_crypto::account::StoredPassword::encode(password, [7; 10]),
            ),
        );
        change(&mut root);
        let path = self.0.join(name);
        fixture::write_tree(&path, &root);
        path
    }

    fn files(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A clean client: a fresh TLS identity, no extra CA, no license.
fn clean_client() -> NativeProvider {
    let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
    let config = NativeConfig {
        client_cert_pem: client.cert.pem().into_bytes(),
        client_key_pem: client.key_pair.serialize_pem().into_bytes(),
        additional_ca_pem: Vec::new(),
        callback_host: "127.0.0.1".into(),
        machine: MachineInfo::default(),
    };
    assert!(config.machine.install_code.is_empty());
    NativeProvider::new(config)
}

fn local(path: &Path, credentials: Option<(&str, &str)>) -> LocalFile {
    LocalFile {
        path: path.to_path_buf(),
        credentials: credentials.map(|(a, p)| (a.to_owned(), p.to_owned().into())),
    }
}

/// `provider` with `path` open as Admin.
fn opened(path: &Path) -> NativeProvider {
    let mut provider = clean_client();
    provider
        .open_local(&local(path, Some((ACCOUNT, PASSWORD))))
        .unwrap();
    provider
}

fn layout(provider: &mut dyn DataProvider, name: &str) -> u32 {
    provider
        .layouts()
        .unwrap()
        .into_iter()
        .find(|layout| layout.name == name)
        .unwrap_or_else(|| panic!("no layout {name}"))
        .id
}

fn text<'p>(page: &'p RecordPage, id: i64, field: &str) -> Option<&'p str> {
    let column = page.fields.iter().position(|f| f.name == field).unwrap();
    page.records.iter().find(|r| r.id == id)?.values[column].as_deref()
}

fn name(value: &str) -> FieldChanges {
    FieldChanges::from([("Name".to_owned(), Some(value.to_owned()))])
}

/// A field with furigana ("Use Furigana Field", option `0x40`): FileMaker
/// Pro writes the reading of the text entered there into another field,
/// which OpenMaker does not (#1301), so a change of its value is refused,
/// saying why, rather than leave the reading as it was (#1302). Another
/// field of the record still changes.
#[test]
fn a_change_of_a_field_with_furigana_is_refused_rather_than_left_without_its_reading() {
    let folder = Folder::new("furigana");
    let path = folder.file(FILE, PASSWORD, |root| {
        fixture::set_field_options(root, TABLE, fixture::NAME, 0x40);
    });
    let mut provider = opened(&path);
    let tasks = layout(&mut provider, "Tasks");
    let before = provider.records(tasks, 0, 10).unwrap();
    let refused = provider
        .update_record(tasks, 1, name("やまだ"))
        .unwrap_err();
    assert_eq!(refused.code, Some(3), "{refused:?}");
    assert!(refused.message.contains("#1302"), "{refused:?}");
    assert_eq!(provider.records(tasks, 0, 10).unwrap(), before);
    let count = FieldChanges::from([("Count".to_owned(), Some("7".to_owned()))]);
    provider.update_record(tasks, 1, count).unwrap();
    let after = provider.records(tasks, 0, 10).unwrap();
    let column = |name: &str| after.fields.iter().position(|f| f.name == name).unwrap();
    let record = after.records.iter().find(|r| r.id == 1).unwrap();
    assert_eq!(record.values[column("Count")].as_deref(), Some("7"));
    let was = before.records.iter().find(|r| r.id == 1).unwrap();
    assert_eq!(record.values[column("Name")], was.values[column("Name")]);
    provider.close();
}

/// Options for Field sets furigana as FileMaker Pro 26.0.3 saved it
/// (#1301): Tasks::Name with Code (field 7) and Full-Width Katakana writes
/// bit `0x40` and member `07`'s second part. A translation Pro was not seen
/// to write, a global field and a number field are refused. While Code
/// receives Name's reading it is not deleted or made a number (its storage
/// is kept by the same guard as its deletion). Turned off, only the bit
/// goes and `07` stays.
#[test]
fn furigana_is_set_and_cleared_as_filemaker_pro_saves_it() {
    use om_data::field_options::Furigana;
    use om_data::SchemaChange;
    let folder = Folder::new("furigana-options");
    let path = folder.file(FILE, PASSWORD, index_schema_catalogs);
    let options_of = |provider: &mut NativeProvider| {
        provider
            .database_schema()
            .unwrap()
            .table(TABLE)
            .and_then(|t| t.fields.iter().find(|f| f.id == fixture::NAME))
            .and_then(|f| f.options.clone())
            .unwrap()
    };
    let set = |options| SchemaChange::SetFieldOptions {
        table_id: TABLE,
        field_id: fixture::NAME,
        options: Box::new(options),
    };
    let on_disk = |path: &Path| {
        let file = om_hbam::HbamFile::open(path).unwrap();
        let tree = file.key_tree().unwrap();
        let (t, f) = (
            om_hbam::slot::uint_segment(TABLE),
            om_hbam::slot::uint_segment(fixture::NAME),
        );
        let value = |key: u8| {
            let path: [&[u8]; 5] = [&t, &[3], &[5], &f, &[key]];
            tree.node(&path)
                .and_then(|n| n.assembled())
                .map(|v| v.to_vec())
        };
        (value(2).unwrap()[8..12].to_vec(), value(7))
    };
    let mut provider = opened(&path);
    provider.begin_schema_edit().unwrap();
    let options = options_of(&mut provider);
    assert_eq!(options.furigana, Furigana::default());
    let with = |field, translation| {
        let mut options = options.clone();
        options.furigana = Furigana {
            on: true,
            field,
            translation,
        };
        options
    };
    let refused = provider.apply_schema_change(set(with(7, 1))).unwrap_err();
    assert_eq!(refused.code, Some(3), "{refused:?}");
    assert!(refused.message.contains("#1334"), "{refused:?}");
    for target in [fixture::SETTING, fixture::COUNT, fixture::NAME] {
        let refused = provider
            .apply_schema_change(set(with(target, 2)))
            .unwrap_err();
        assert!(
            refused.message.contains("text field"),
            "{target}: {refused:?}"
        );
    }
    provider.apply_schema_change(set(with(7, 2))).unwrap();
    provider.commit_schema_changes().unwrap();
    provider.close();
    let member = om_hbam::slot::uint_segment(7);
    let mut expected = vec![0x02, 0x01, 0x00, 0x00, 0x00, 0x00, 0x02, 0x01];
    expected.extend(&member);
    expected.extend([0x00, 0x00, 0x00, 0x02]);
    assert_eq!(
        on_disk(&path),
        (vec![0, 0, 0, 0x40], Some(expected.clone()))
    );

    let mut provider = opened(&path);
    provider.begin_schema_edit().unwrap();
    for change in [
        SchemaChange::DeleteField {
            table_id: TABLE,
            field_id: 7,
        },
        SchemaChange::ChangeField {
            table_id: TABLE,
            field_id: 7,
            name: "Code".into(),
            data_type: om_data::FieldKind::Number,
            comment: String::new(),
        },
    ] {
        let refused = provider.apply_schema_change(change).unwrap_err();
        assert!(refused.message.contains("furigana"), "{refused:?}");
    }
    let mut options = options_of(&mut provider);
    assert_eq!(options.furigana, with(7, 2).furigana);
    options.furigana.on = false;
    provider.apply_schema_change(set(options)).unwrap();
    provider.commit_schema_changes().unwrap();
    provider.close();
    assert_eq!(on_disk(&path), (vec![0, 0, 0, 0], Some(expected)));
}

/// A copy of a FileMaker Pro starter solution opens with no account typed,
/// as FileMaker Pro opens it: its File Options sign in as Admin with the
/// empty password (local-files.md 3.1).
#[test]
#[ignore = "requires OM_STARTER_FILE, a writable copy of a FileMaker Pro starter solution"]
fn a_starter_solution_opens_without_asking() {
    let path = PathBuf::from(std::env::var_os("OM_STARTER_FILE").expect("a file"));
    let mut provider = clean_client();
    assert_eq!(
        provider.open_local(&local(&path, None)).unwrap(),
        LocalOpen::Opened { read_only: None }
    );
    provider.close();
}

/// File Options' "Log in using" (local-files.md 3.1): with the account's
/// right password stored, the file opens without asking, as FileMaker Pro
/// opens the starter solutions; held Option or Shift
/// (`open_local_asking`) asks anyway; a stored password the account no
/// longer has asks, suggesting the account, as FileMaker Pro asks again.
#[test]
fn file_options_sign_in_opens_without_asking() {
    let folder = Folder::new("file-options");
    let stored = |password: &str| {
        let password = password.to_owned();
        move |root: &mut om_hbam::OwnedNode| {
            root.put(
                &[&[23], &[1], &[1], &[6]],
                om_scripts::text_node(&[], &password),
            );
        }
    };
    let path = folder.file("Right.fmp12", PASSWORD, stored(PASSWORD));
    let mut provider = clean_client();
    assert_eq!(
        provider.open_local(&local(&path, None)).unwrap(),
        LocalOpen::Opened { read_only: None }
    );
    provider.close();
    let mut provider = clean_client();
    assert_eq!(
        provider.open_local_asking(&path).unwrap(),
        LocalOpen::NeedsLogin {
            account: ACCOUNT.into()
        }
    );
    provider.close();
    let path = folder.file("Wrong.fmp12", PASSWORD, stored("not it"));
    let mut provider = clean_client();
    assert_eq!(
        provider.open_local(&local(&path, None)).unwrap(),
        LocalOpen::NeedsLogin {
            account: ACCOUNT.into()
        }
    );
    // The file waits for the account typed.
    assert_eq!(
        provider
            .open_local(&local(&path, Some((ACCOUNT, PASSWORD))))
            .unwrap(),
        LocalOpen::Opened { read_only: None }
    );
    provider.close();
}

#[test]
fn a_local_file_asks_for_an_account_saves_an_edit_and_reopens_with_it() {
    let folder = Folder::new("edit");
    let path = folder.tasks(None);
    let mut provider = clean_client();
    assert!(provider.supports_local_files());
    // It always asks first, suggesting the account the file names.
    assert_eq!(
        provider.open_local(&local(&path, None)).unwrap(),
        LocalOpen::NeedsLogin {
            account: ACCOUNT.into()
        }
    );
    let refused = provider
        .open_local(&local(&path, Some((ACCOUNT, "wrong"))))
        .unwrap_err();
    assert_eq!(refused.code, Some(212), "{refused:?}");
    assert_eq!(
        provider
            .open_local(&local(&path, Some((ACCOUNT, PASSWORD))))
            .unwrap(),
        LocalOpen::Opened { read_only: None }
    );
    // A file that is not hosted has no host version.
    assert_eq!(provider.host_application_version(), None);
    assert_eq!(provider.session_account().as_deref(), Some(ACCOUNT));

    let tasks = layout(&mut provider, "Tasks");
    let page = provider.records(tasks, 0, 10).unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(text(&page, 1, "Name"), Some("Write tests"));
    assert!(provider.record_capabilities(tasks).unwrap().update);
    let saved = provider
        .update_record(tasks, 1, name("Tested locally"))
        .unwrap();
    assert_eq!(saved.id, 1);
    let created = provider.create_record(tasks, name("New here")).unwrap();
    provider.close();
    // Closed: nothing holds the file, and only it is in its folder.
    assert_eq!(folder.files(), [FILE]);

    let mut reopened = opened(&path);
    let tasks = layout(&mut reopened, "Tasks");
    let page = reopened.records(tasks, 0, 10).unwrap();
    assert_eq!(page.total, 3);
    assert_eq!(text(&page, 1, "Name"), Some("Tested locally"));
    assert_eq!(text(&page, 2, "Name"), Some("Ship"));
    assert_eq!(text(&page, created.id, "Name"), Some("New here"));
}

#[test]
fn an_account_without_a_password_asks_when_held_and_refused_accounts_change_nothing() {
    let folder = Folder::new("refusals");
    let path = folder.tasks(Some(""));
    let before = std::fs::read(&path).unwrap();
    let mut provider = clean_client();
    // Admin's password is empty and File Options sign in as Admin: asked
    // for only when the user holds Shift or Option (local-files.md 3.1).
    assert_eq!(
        provider.open_local_asking(&path).unwrap(),
        LocalOpen::NeedsLogin {
            account: ACCOUNT.into()
        }
    );
    // Clerk's password is right, but its set is not one OpenMaker enforces:
    // a refusal of its own, the file still waiting for another account.
    let clerk = provider
        .open_local(&local(&path, Some((CLERK, CLERK_PASSWORD))))
        .unwrap_err();
    assert_eq!(clerk.code, Some(9), "{clerk:?}");
    assert!(
        clerk.message.contains("does not enforce yet"),
        "{}",
        clerk.message
    );
    // Newcomer's password must be changed first: refused saying so, the
    // file still waiting (for the new password, #807).
    let newcomer = provider
        .open_local(&local(&path, Some((NEWCOMER, NEWCOMER_PASSWORD))))
        .unwrap_err();
    assert_eq!(newcomer.code, Some(211), "{newcomer:?}");
    assert!(
        newcomer.message.contains("must be changed"),
        "{}",
        newcomer.message
    );
    provider.close();
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(
        provider
            .open_local(&local(&path, Some((ACCOUNT, ""))))
            .unwrap(),
        LocalOpen::Opened { read_only: None }
    );
    assert_eq!(provider.session_account().as_deref(), Some(ACCOUNT));
    assert_eq!(provider.account_info().password_expiry, Some(-1));
    provider.close();
    // Not held: File Options sign in as Admin without asking.
    let mut provider = clean_client();
    assert_eq!(
        provider.open_local(&local(&path, None)).unwrap(),
        LocalOpen::Opened { read_only: None }
    );
    assert_eq!(provider.session_account().as_deref(), Some(ACCOUNT));
}

#[test]
fn a_file_open_elsewhere_is_refused_until_it_closes() {
    let folder = Folder::new("in-use");
    let path = folder.tasks(None);
    let mut first = opened(&path);
    let mut second = clean_client();
    let in_use = second
        .open_local(&local(&path, Some((ACCOUNT, PASSWORD))))
        .unwrap_err();
    assert_eq!(in_use.code, Some(300), "{in_use:?}");
    // The first window's file is untouched by the refusal.
    let tasks = layout(&mut first, "Tasks");
    first.update_record(tasks, 2, name("Still mine")).unwrap();
    first.close();
    second
        .open_local(&local(&path, Some((ACCOUNT, PASSWORD))))
        .unwrap();
    let tasks = layout(&mut second, "Tasks");
    let page = second.records(tasks, 0, 10).unwrap();
    assert_eq!(text(&page, 2, "Name"), Some("Still mine"));
}

#[test]
fn a_file_another_program_replaced_is_not_saved_over() {
    let folder = Folder::new("replaced");
    let path = folder.tasks(None);
    let mut provider = opened(&path);
    let tasks = layout(&mut provider, "Tasks");
    // Another program puts a file of its own at the path.
    let theirs = folder.0.join("theirs");
    std::fs::write(&theirs, b"another program's file").unwrap();
    std::fs::rename(&theirs, &path).unwrap();
    assert!(provider.update_record(tasks, 1, name("Not saved")).is_err());
    provider.close();
    assert_eq!(std::fs::read(&path).unwrap(), b"another program's file");
    assert_eq!(folder.files(), [FILE]);
}

#[cfg(unix)]
#[test]
fn a_save_never_writes_through_an_entry_it_did_not_make() {
    let folder = Folder::new("copy-name");
    let path = folder.tasks(None);
    let mut provider = opened(&path);
    let tasks = layout(&mut provider, "Tasks");
    let before = std::fs::read(&path).unwrap();
    // After the file opened, a link appears where the save's new copy
    // goes: to another file, then to the database itself.
    let copy = folder.0.join(format!("{FILE}.omsave"));
    let victim = folder.0.join("victim");
    std::fs::write(&victim, b"not to be truncated").unwrap();
    for target in [&victim, &path] {
        std::os::unix::fs::symlink(target, &copy).unwrap();
        assert!(provider.update_record(tasks, 1, name("Not saved")).is_err());
        assert!(std::fs::symlink_metadata(&copy)
            .unwrap()
            .file_type()
            .is_symlink());
        std::fs::remove_file(&copy).unwrap();
    }
    std::fs::hard_link(&victim, &copy).unwrap();
    assert!(provider.update_record(tasks, 1, name("Not saved")).is_err());
    assert!(copy.is_file());
    provider.close();
    assert_eq!(std::fs::read(&victim).unwrap(), b"not to be truncated");
    assert_eq!(std::fs::read(&copy).unwrap(), b"not to be truncated");
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn damaged_files_are_refused_unchanged() {
    let folder = Folder::new("damaged");
    let path = folder.tasks(None);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.truncate(bytes.len() - 1000);
    std::fs::write(&path, &bytes).unwrap();
    let mut provider = clean_client();
    let error = provider
        .open_local(&local(&path, Some((ACCOUNT, PASSWORD))))
        .unwrap_err();
    assert_eq!(error.code, Some(805), "{error:?}");
    assert!(error.message.contains(FILE), "{}", error.message);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert_eq!(folder.files(), [FILE]);
    // Nothing stays open: the next file opens on the same provider.
    let other = Folder::new("damaged-next");
    provider
        .open_local(&local(&other.tasks(None), Some((ACCOUNT, PASSWORD))))
        .unwrap();
}

/// A page setup accepted in Page Setup is kept in a file on this computer,
/// as FileMaker Pro keeps it at the OK (#112). `[2, 87]` then holds the one
/// `Comm` chunk, stored chunked as Pro stores it: child `00` the length,
/// then the piece (file-format.md 3.8; Pro read such a file whole), and
/// the file opens with it. What the file stores is replaced only when it is
/// what the caller read, and holds only records FileMaker Pro makes again;
/// otherwise it is kept (AGENTS.md rule 5).
#[test]
fn a_page_setup_is_kept_in_the_local_file_as_filemaker_pro_stores_it() {
    use om_layout::paper::{Orientation, PaperSettings};
    let folder = Folder::new("page-setup");
    let path = folder.tasks(None);
    let value = PaperSettings {
        width: 595.2756,
        height: 841.8898,
        orientation: Orientation::Landscape,
        scale: 0.5,
    }
    .common_format();
    let stored = |path: &Path| {
        let file = om_hbam::HbamFile::open(path).unwrap();
        let tree = file.key_tree().unwrap();
        tree.node(&[&[2], &[0x87]]).map(|node| {
            let parts: Vec<(Vec<u8>, Option<Vec<u8>>)> = node
                .children
                .iter()
                .map(|child| (child.segment.0.to_vec(), child.value.map(<[u8]>::to_vec)))
                .collect();
            parts
        })
    };
    assert_eq!(stored(&path), None);

    let mut provider = opened(&path);
    assert!(provider.store_page_format(None, &value).unwrap());
    // The file now stores a setup: one saved over "none" again is refused.
    let changed = provider.store_page_format(None, &value).unwrap_err();
    assert_eq!(changed.code, Some(301), "{changed:?}");
    provider.close();
    assert_eq!(
        stored(&path),
        Some(vec![
            (vec![0], Some(37u32.to_be_bytes().to_vec())),
            (vec![1], Some(value.to_vec())),
        ])
    );
    let mut reopened = opened(&path);
    let schema = reopened.calc_schema().unwrap().unwrap();
    assert_eq!(schema.page_format.as_deref(), Some(&value[..]));
    reopened.close();

    // Kept: a record FileMaker Pro does not make from `Comm` beside it,
    // and a value that does not read (declared larger than 1 MiB).
    let mut unknown = value.to_vec();
    unknown.extend_from_slice(b"W_XX\x00\x00\x00\x01\x07");
    let mut unreadable = om_data::meta::page_format_node(&value).unwrap();
    unreadable.children[0].value = Some((2u32 << 20).to_be_bytes().to_vec());
    for (name, node, expected) in [
        (
            "unknown.fmp12",
            om_data::meta::page_format_node(&unknown).unwrap(),
            Some(&unknown[..]),
        ),
        ("unreadable.fmp12", unreadable, None),
    ] {
        let path = folder.file(name, PASSWORD, |root| root.put(&[&[2], &[0x87]], node));
        let before = stored(&path);
        let mut provider = opened(&path);
        let kept = provider.store_page_format(expected, &value).unwrap_err();
        assert_eq!(kept.code, Some(3), "{name}: {kept:?}");
        provider.close();
        assert_eq!(stored(&path), before, "{name}");
    }
}

#[cfg(unix)]
#[test]
fn a_file_it_may_not_write_opens_read_only_and_refuses_every_change() {
    use std::os::unix::fs::PermissionsExt;
    let folder = Folder::new("read-only");
    let path = folder.tasks(None);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    if std::fs::OpenOptions::new().write(true).open(&path).is_ok() {
        eprintln!("skipped: this user writes read-only files");
        return;
    }
    let before = std::fs::read(&path).unwrap();
    let mut provider = clean_client();
    let opened = provider
        .open_local(&local(&path, Some((ACCOUNT, PASSWORD))))
        .unwrap();
    assert!(
        matches!(&opened, LocalOpen::Opened { read_only: Some(reason) } if reason.contains("may not")),
        "{opened:?}"
    );
    let tasks = layout(&mut provider, "Tasks");
    let page = provider.records(tasks, 0, 10).unwrap();
    assert_eq!(text(&page, 1, "Name"), Some("Write tests"));
    let capabilities = provider.record_capabilities(tasks).unwrap();
    assert!(!capabilities.create && !capabilities.update && !capabilities.delete);
    for refused in [
        provider.update_record(tasks, 1, name("No")).map(|_| ()),
        provider.create_record(tasks, name("No")).map(|_| ()),
        provider.delete_record(tasks, 1),
        // The scripts' and portals' deletion, also inside a transaction.
        provider.delete_related_record(TABLE, 2),
    ] {
        assert_eq!(refused.unwrap_err().code, Some(6));
    }
    provider
        .begin_transaction(TransactionOptions::default())
        .unwrap();
    assert_eq!(
        provider.delete_related_record(TABLE, 2).unwrap_err().code,
        Some(6)
    );
    provider.revert_transaction().unwrap();
    let page = provider.records(tasks, 0, 10).unwrap();
    assert_eq!(page.total, 2);
    provider.close();
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(folder.files(), [FILE]);
}

/// OpenMaker Server hosting `path` with the file's Admin account, recording
/// its connections in `record_dir`; the CA to trust.
fn om_server(path: &Path, record_dir: &Path) -> (u16, Vec<u8>) {
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
        record_dir: Some(record_dir.to_path_buf()),
    };
    let server = Server::bind("127.0.0.1:0", config, identity).unwrap();
    // Reads through views are answered through the test view too, and
    // checked against the file engine's (docs/server-storage.md 5, "Views").
    #[cfg(debug_assertions)]
    let server = server.checking_reads();
    let port = server.local_addr().unwrap().port();
    std::thread::spawn(move || server.serve());
    (port, ca_pem)
}

/// The install codes (client licenses) of every Connect the server
/// recorded.
fn recorded_install_codes(dir: &Path) -> Vec<String> {
    let mut codes = Vec::new();
    for connection in std::fs::read_dir(dir).unwrap() {
        let stream = std::fs::read(connection.unwrap().path().join("c2s.bin")).unwrap();
        let mut at = 0;
        while let Ok((_, length)) = om_giop::parse_header(&stream[at..]) {
            let message = &stream[at..at + length];
            at += length;
            if om_giop::Request::parse(message)
                .is_ok_and(|r| r.operation == om_protocol::op::CONNECT)
            {
                let connect = om_protocol::parse_connect_request(message).unwrap();
                codes.push(connect.machine.install_code);
            }
        }
    }
    codes
}

#[test]
fn openmaker_server_takes_a_client_without_a_license() {
    let folder = Folder::new("om-server");
    let path = folder.tasks(None);
    let recordings = folder.0.join("recordings");
    let (port, ca_pem) = om_server(&path, &recordings);
    let clean = |ca_pem: &[u8]| {
        let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
        let mut provider = NativeProvider::new(NativeConfig {
            client_cert_pem: client.cert.pem().into_bytes(),
            client_key_pem: client.key_pair.serialize_pem().into_bytes(),
            additional_ca_pem: ca_pem.to_vec(),
            callback_host: "127.0.0.1".into(),
            machine: MachineInfo::default(),
        });
        // What the desktop's worker does with Use saved license off, or
        // with no saved license.
        provider.set_client_license(None).unwrap();
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
    };
    let mut provider = clean(&ca_pem);
    assert_eq!(
        provider.account_info(),
        om_data::host::AccountInfo {
            set_access: Some(om_data::host::security::SetAccess::BuiltIn(
                om_data::host::security::BuiltInSet::FullAccess,
            )),
            ..Default::default()
        },
        "the authenticated set reply proves access, not account type or expiry"
    );
    let tasks = layout(&mut provider, "Tasks");
    provider
        .update_record(tasks, 1, name("Saved on the server"))
        .unwrap();
    provider.close();
    // Written to the hosted file: a second unlicensed client reads it.
    let mut again = clean(&ca_pem);
    let tasks = layout(&mut again, "Tasks");
    let page = again.records(tasks, 0, 10).unwrap();
    assert_eq!(text(&page, 1, "Name"), Some("Saved on the server"));
    again.close();

    let codes = recorded_install_codes(&recordings);
    assert!(!codes.is_empty());
    assert!(
        codes.iter().all(String::is_empty),
        "a license was sent: {} of {} Connects",
        codes.iter().filter(|c| !c.is_empty()).count(),
        codes.len()
    );
}

#[test]
fn local_reconnect_keeps_its_private_engine_and_each_windows_found_set() {
    use om_data::found::FoundSetChange;
    let folder = Folder::new("reconnect");
    let path = folder.tasks(None);
    let mut provider = opened(&path);
    let tasks = layout(&mut provider, "Tasks");
    let before = provider.records(tasks, 0, 10).unwrap();
    let first = before.records[0].id;
    let second = before.records[1].id;
    let port = provider.server_port();
    provider
        .change_found_set(tasks, FoundSetChange::Omit(vec![first]))
        .unwrap();
    provider.copy_window(1, 2);
    provider.use_window(2);
    provider
        .change_found_set(tasks, FoundSetChange::ShowAll)
        .unwrap();
    provider.use_window(1);
    assert_eq!(
        provider
            .reopen_local(&local(&path, Some((ACCOUNT, "wrong"))))
            .unwrap_err()
            .code,
        Some(212)
    );
    // A refused login keeps exclusive ownership, rather than dropping and
    // re-opening the engine or letting another process acquire the file.
    let mut other = clean_client();
    assert_eq!(
        other.open_local(&local(&path, None)).unwrap_err().code,
        Some(300)
    );
    provider
        .reopen_local(&local(&path, Some((ACCOUNT, PASSWORD))))
        .unwrap();
    assert_eq!(provider.server_port(), port);
    let page = provider.records(tasks, 0, 10).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.records[0].id, second);
    provider.use_window(2);
    assert_eq!(provider.records(tasks, 0, 10).unwrap().total, 2);
    provider.use_window(1);
    provider.record_by_id(tasks, second).unwrap();
    provider
        .update_record(tasks, second, name("Recovered locally"))
        .unwrap();
    provider.close();
    let mut reopened = opened(&path);
    let tasks = layout(&mut reopened, "Tasks");
    let page = reopened.record_by_id(tasks, second).unwrap();
    assert_eq!(text(&page, second, "Name"), Some("Recovered locally"));
    reopened.close();
    assert_eq!(folder.files(), [FILE]);
}

/// A local file opens on the found set FileMaker Pro saved for the layout's
/// occurrence when it closed the file (`[TO, 07, 01]`, file-metadata.md
/// 11): its records that still exist, sorted again by its sort, with the
/// window told once where its current record is.
#[test]
fn a_local_file_opens_on_the_found_set_and_record_it_was_closed_with() {
    use om_data::found::FoundSet;
    use om_hbam::OwnedNode;
    use om_scripts::specs::{SortEntry, SortSpec, SpecField};
    let folder = Folder::new("saved-found");
    let path = folder.file(FILE, PASSWORD, |root| {
        fixture::add_tasks(root, 3..=5);
        // Records 2, 4, 5 and 9 (since deleted), record 4 current, sorted
        // by Name descending.
        let mut found = om_hbam::RidList::new();
        for id in [2, 4, 5, 9] {
            found.insert(id);
        }
        let sort = SortSpec {
            keep_sorted: true,
            blanks_last: false,
            fields: vec![SortEntry {
                field: SpecField {
                    occurrence: Some(fixture::OCCURRENCE),
                    field: fixture::NAME,
                    repetition: 1,
                },
                summary: None,
                value_list: None,
                descending: true,
                locale: 0,
            }],
        };
        let occurrence = om_hbam::slot::uint_segment(fixture::OCCURRENCE);
        root.put(
            &[&occurrence, &[7], &[1]],
            OwnedNode::list(
                Vec::new(),
                vec![
                    OwnedNode::leaf([0x02], vec![1]),
                    OwnedNode::leaf([0x03], vec![1, 4]),
                    found.to_node(vec![0x05]),
                    OwnedNode::leaf([0x0a], sort.encode()),
                ],
            ),
        );
    });
    let mut provider = opened(&path);
    let tasks = layout(&mut provider, "Tasks");
    let saved = FoundSet {
        found: 3,
        total: 5,
        sorted: true,
    };
    let opened_on = |provider: &mut NativeProvider| {
        let (page, summary) = provider.records_with_summary(tasks, 0, 10, &[]).unwrap();
        let ids: Vec<i64> = page.records.iter().map(|r| r.id).collect();
        assert_eq!(ids, [5, 4, 2], "Task 5, Task 4, Ship");
        assert_eq!(summary, Some(saved));
        assert_eq!(provider.take_saved_position(tasks), Some(1));
        assert_eq!(provider.take_saved_position(tasks), None, "told once");
    };
    opened_on(&mut provider);
    // Show All is the window's own: the saved set is not read again.
    provider
        .change_found_set(tasks, om_data::found::FoundSetChange::ShowAll)
        .unwrap();
    let (page, summary) = provider.records_with_summary(tasks, 0, 10, &[]).unwrap();
    assert_eq!((page.total, summary.unwrap().found), (5, 5));
    // Another window that has not read the table starts on it too.
    provider.use_window(2);
    opened_on(&mut provider);
    // A record a window makes before it reads the table joins the saved
    // set it starts on.
    provider.use_window(3);
    let made = provider
        .create_record(tasks, name("Made first"))
        .unwrap()
        .id;
    let page = provider.records(tasks, 0, 10).unwrap();
    let ids: Vec<i64> = page.records.iter().map(|r| r.id).collect();
    assert_eq!(ids, [5, 4, 2, made]);
    // Window 1's Show All has every record, the new one too.
    provider.use_window(1);
    assert_eq!(provider.records(tasks, 0, 10).unwrap().total, 6);
    provider.close();
}

/// A window that wrote a record of a local file keeps its found set and
/// record in the file when it closes (`[TO, 07]`, file-metadata.md 11), in
/// the bytes FileMaker Pro writes, and the file opens on them; a window that
/// wrote nothing keeps nothing.
#[test]
fn a_window_that_wrote_a_record_keeps_its_found_set_when_it_closes() {
    use om_data::found::FoundSetChange;
    use om_hbam::OwnedNode;
    let folder = Folder::new("kept-found");
    let path = folder.file(FILE, PASSWORD, |root| {
        fixture::add_tasks(root, 3..=5);
        // The table's data version, as every file has it.
        let table = om_hbam::slot::uint_segment(TABLE);
        root.put(
            &[&[3], &[0x10], &[5], &table, &[0xfb]],
            OwnedNode::leaf(Vec::new(), 2u32.to_be_bytes().to_vec()),
        );
    });
    let found_on = |path: &Path| {
        let mut provider = opened(path);
        let tasks = layout(&mut provider, "Tasks");
        let page = provider.records(tasks, 0, 10).unwrap();
        let ids: Vec<i64> = page.records.iter().map(|r| r.id).collect();
        let at = provider.take_saved_position(tasks);
        provider.close();
        (ids, at)
    };
    let omit_and_close = |write: bool| {
        let mut provider = opened(&path);
        let tasks = layout(&mut provider, "Tasks");
        provider.records(tasks, 0, 10).unwrap();
        provider
            .change_found_set(tasks, FoundSetChange::Omit(vec![1, 4]))
            .unwrap();
        if write {
            provider.record_by_id(tasks, 3).unwrap();
            provider.update_record(tasks, 3, name("Kept")).unwrap();
        }
        provider.keep_found_set(tasks, Some(3));
        provider.close();
    };
    omit_and_close(false);
    assert_eq!(found_on(&path), (vec![1, 2, 3, 4, 5], None), "nothing kept");
    omit_and_close(true);
    assert_eq!(found_on(&path), (vec![2, 3, 5], Some(1)));
    // Records 2, 3, 5 (one bitmap word), record 3 current, data version 2.
    let file = om_hbam::HbamFile::open(&path).unwrap();
    let tree = file.key_tree().unwrap();
    let occurrence = om_hbam::slot::uint_segment(fixture::OCCURRENCE);
    let kept = tree.node(&[&occurrence, &[7], &[1]]).unwrap();
    let member = |key: u8| kept.child(&[key]).and_then(|n| n.value).map(<[u8]>::to_vec);
    assert_eq!(member(0x03), Some(vec![1, 3]));
    let rids = kept.child(&[0x05]).unwrap();
    assert_eq!(rids.children[0].value, Some(&[0, 0, 0, 0x2c][..]));
    let fe = kept.child(&[0xfe]).unwrap().children[0].value;
    assert_eq!(fe, Some(&[0, 0, 0, 2][..]));
    assert!(kept.child(&[0xff]).is_none() && kept.child(&[0x02]).is_none());
}

/// A first Quick Find on a field whose index the file lacks has the host
/// build it: the file changed, so the window keeps its found set when it
/// closes, as FileMaker Pro 26.0.3 kept it after a Quick Find on the Tasks
/// and Assets starters (#536). A later find, its index built, changes
/// nothing and keeps nothing (file-metadata.md 11).
#[test]
fn a_window_whose_find_built_an_index_keeps_its_found_set_when_it_closes() {
    use om_data::found::{FindRequest, FoundSetChange};
    use om_hbam::OwnedNode;
    let folder = Folder::new("found-kept");
    let path = folder.file(FILE, PASSWORD, |root| {
        fixture::add_tasks(root, 3..=5);
        let table = om_hbam::slot::uint_segment(TABLE);
        root.put(
            &[&[3], &[0x10], &[5], &table, &[0xfb]],
            OwnedNode::leaf(Vec::new(), 2u32.to_be_bytes().to_vec()),
        );
    });
    let quick_find_and_close = |path: &Path, word: &str| {
        let mut provider = opened(path);
        let tasks = layout(&mut provider, "Tasks");
        provider.records(tasks, 0, 10).unwrap();
        let request = FindRequest {
            omit: false,
            criteria: vec![("Name".into(), word.into())],
        };
        let change = FoundSetChange::QuickFind(vec![vec![request]]);
        provider.change_found_set(tasks, change).unwrap();
        provider.keep_found_set(tasks, Some(4));
        provider.close();
    };
    let found_on = |path: &Path| {
        let mut provider = opened(path);
        let tasks = layout(&mut provider, "Tasks");
        let page = provider.records(tasks, 0, 10).unwrap();
        let ids: Vec<i64> = page.records.iter().map(|r| r.id).collect();
        let at = provider.take_saved_position(tasks);
        provider.close();
        (ids, at)
    };
    quick_find_and_close(&path, "Task");
    assert_eq!(found_on(&path), (vec![3, 4, 5], Some(1)));
    quick_find_and_close(&path, "Task 5");
    assert_eq!(found_on(&path), (vec![3, 4, 5], Some(1)), "nothing changed");
    // A field whose options prohibit an index is never given one: its find
    // changes nothing, every time.
    let unindexed = folder.file("Unindexed.fmp12", PASSWORD, |root| {
        fixture::add_tasks(root, 3..=5);
        fixture::set_field_options(root, TABLE, fixture::NAME, 0x4000_0000);
        let table = om_hbam::slot::uint_segment(TABLE);
        root.put(
            &[&[3], &[0x10], &[5], &table, &[0xfb]],
            OwnedNode::leaf(Vec::new(), 2u32.to_be_bytes().to_vec()),
        );
    });
    quick_find_and_close(&unindexed, "Task");
    assert_eq!(found_on(&unindexed), (vec![1, 2, 3, 4, 5], None));
}

/// Occurrence of the related file's Tasks table in the file that names it.
const PEER_ROWS: u64 = 0xd0_0040;

fn name_of(provider: &mut dyn DataProvider, layout: u32, id: i64) -> Option<String> {
    let page = provider.record_by_id(layout, id).unwrap();
    text(&page, id, "Name").map(str::to_owned)
}

#[test]
fn a_variable_source_opens_on_first_read_and_keeps_its_file() {
    use om_data::calc::{eval::Variables, Value};
    use om_data::host::CalcNeed;

    let folder = Folder::new("variable-source");
    let root_path = folder.file("Root.fmp12", PASSWORD, |root| {
        related::reference_with(
            root,
            (PEER_ROWS, "Peer rows"),
            0x8001,
            "Peer",
            &[("", "", "$$RelatedPath")],
        );
    });
    folder.file("First.fmp12", PASSWORD, |_| {});
    folder.file("Second.fmp12", PASSWORD, |_| {});
    let mut root = clean_client();
    root.open_local(&local(&root_path, Some((ACCOUNT, PASSWORD))))
        .unwrap();
    assert!(root.calc_schema().unwrap().unwrap().is_pending(PEER_ROWS));
    assert!(root.related_file_warnings().is_empty());
    assert!(!root.references_peer("First").unwrap());

    let mut variables = Variables::new();
    variables.set("$$RelatedPath", 1, Value::text("file:First.fmp12"));
    root.set_file_variables(&variables);
    assert_eq!(
        root.local_source_variable("$$RelatedPath"),
        Some("file:First.fmp12")
    );
    let needs = [CalcNeed::Occurrence {
        occurrence: PEER_ROWS,
    }];
    root.calc_data(&needs).unwrap();
    assert!(
        root.references_peer("First").unwrap(),
        "{:?}",
        root.related_file_warnings()
    );
    let first = root
        .calc_schema()
        .unwrap()
        .unwrap()
        .occurrence(PEER_ROWS)
        .unwrap()
        .table;

    variables.set("$$RelatedPath", 1, Value::text("file:Second.fmp12"));
    root.set_file_variables(&variables);
    root.calc_data(&needs).unwrap();
    assert_eq!(
        root.calc_schema()
            .unwrap()
            .unwrap()
            .occurrence(PEER_ROWS)
            .unwrap()
            .table,
        first
    );
    assert!(!root.references_peer("Second").unwrap());
}

/// One window's family of files on this computer: Root's occurrences show
/// Peer's table, a script of Root opens Peer, and both write. Each file is
/// served by one engine whatever opened it, writes go to the file that owns
/// them, and each close gives up only its own hold: the last one releases
/// the file to other processes.
#[test]
fn related_and_script_opened_local_files_share_one_engine_and_close_by_lease() {
    let folder = Folder::new("family");
    let root_path = folder.file("Root.fmp12", PASSWORD, |root| {
        related::reference(root, (PEER_ROWS, "Peer rows"), 0x8001, "Peer");
    });
    let peer_path = folder.file("Peer.fmp12", PASSWORD, |_| {});
    let mut root = opened(&root_path);
    // The related file opens beside Root with Root's account: its table
    // is Root's occurrence's, read from Peer.
    let schema = root.calc_schema().unwrap().unwrap();
    assert!(
        root.related_file_warnings().is_empty(),
        "{:?}",
        root.related_file_warnings()
    );
    let table = schema.occurrence(PEER_ROWS).unwrap().table;
    assert!(
        schema.table_context(table).is_some(),
        "bound to Peer's table"
    );
    let data = root
        .calc_data(&[om_data::host::CalcNeed::Record { table, record: 1 }])
        .unwrap();
    let locale = om_data::calc::Locale::en_us();
    let cx = om_data::calc::Ctx::new(&locale, om_data::calc::Date::from_ymd(2026, 1, 1).unwrap());
    let related = data.records[&(table, 1)].as_ref().unwrap();
    assert_eq!(
        related.get(fixture::NAME, 1).unwrap().to_text(&cx).as_str(),
        "Write tests"
    );
    assert!(root.references_peer("peer").unwrap());
    // Another window (another family) finds both files in use.
    let mut elsewhere = clean_client();
    for path in [&root_path, &peer_path] {
        let refused = elsewhere
            .open_local(&local(path, Some((ACCOUNT, PASSWORD))))
            .unwrap_err();
        assert_eq!(refused.code, Some(300), "{refused:?}");
    }

    // A script's Open File: the related file's engine, a session of its own.
    let mut peer = root.open_peer("Peer").unwrap();
    assert_eq!(
        peer.local_path(),
        Some(std::fs::canonicalize(&peer_path).unwrap())
    );
    assert_eq!(peer.session_account().as_deref(), Some(ACCOUNT));
    let tasks = layout(&mut root, "Tasks");
    let peer_tasks = layout(&mut *peer, "Tasks");
    peer.update_record(peer_tasks, 1, name("Written by Peer"))
        .unwrap();
    root.update_record(tasks, 2, name("Written by Root"))
        .unwrap();

    // Root closes: its related reader goes with it, but Peer's engine stays
    // with the session the script opened, still writing.
    root.close();
    assert_eq!(
        name_of(&mut *peer, peer_tasks, 1).as_deref(),
        Some("Written by Peer")
    );
    peer.update_record(peer_tasks, 2, name("Peer after Root closed"))
        .unwrap();
    elsewhere
        .open_local(&local(&root_path, Some((ACCOUNT, PASSWORD))))
        .unwrap();
    let elsewhere_tasks = layout(&mut elsewhere, "Tasks");
    assert_eq!(
        name_of(&mut elsewhere, elsewhere_tasks, 2).as_deref(),
        Some("Written by Root")
    );
    assert_eq!(
        name_of(&mut elsewhere, elsewhere_tasks, 1).as_deref(),
        Some("Write tests")
    );
    elsewhere.close();
    let mut other = clean_client();
    assert_eq!(
        other
            .open_local(&local(&peer_path, Some((ACCOUNT, PASSWORD))))
            .unwrap_err()
            .code,
        Some(300)
    );

    // The last hold goes: Peer is free, with both of its writes.
    peer.close();
    other
        .open_local(&local(&peer_path, Some((ACCOUNT, PASSWORD))))
        .unwrap();
    let other_tasks = layout(&mut other, "Tasks");
    assert_eq!(
        name_of(&mut other, other_tasks, 1).as_deref(),
        Some("Written by Peer")
    );
    assert_eq!(
        name_of(&mut other, other_tasks, 2).as_deref(),
        Some("Peer after Root closed")
    );
    other.close();
    assert_eq!(folder.files(), ["Peer.fmp12", "Root.fmp12"]);
}

/// What a family of local files refuses, each leaving the opener as it was:
/// a missing file (100), a file whose own accounts refuse the opener's
/// account (212), a name with a folder (3), a second file of an open
/// file's name reached by a link (802). A link to an open file is that file.
/// Reconnecting retains credentials without bypassing a target's accounts.
#[cfg(unix)]
#[test]
fn local_peers_are_refused_when_missing_refused_or_ambiguous() {
    let folder = Folder::new("refusals-family");
    let elsewhere = Folder::new("refusals-elsewhere");
    let root_path = folder.file("Root.fmp12", PASSWORD, |root| {
        related::reference(root, (PEER_ROWS, "Gone rows"), 0x8001, "Gone");
        related::reference(root, (0xd0_0041, "Guarded rows"), 0x8002, "Guarded");
    });
    folder.file("Guarded.fmp12", "another password", |_| {});
    let peer_path = folder.file("Peer.fmp12", PASSWORD, |_| {});
    // Another Peer.fmp12, reached from Root's folder through a link.
    let twin = elsewhere.file("Peer.fmp12", PASSWORD, |_| {});
    std::os::unix::fs::symlink(&twin, folder.0.join("Twin.fmp12")).unwrap();
    std::os::unix::fs::symlink(&peer_path, folder.0.join("Alias.fmp12")).unwrap();
    let before = std::fs::read(&root_path).unwrap();

    let mut root = opened(&root_path);
    root.calc_schema().unwrap();
    let warnings = root.related_file_warnings().join("\n");
    assert!(
        warnings
            .contains("Related file “Gone” (data source “Gone” of “Root”): there is no such file"),
        "{warnings}"
    );
    assert!(
        warnings
            .contains("Related file “Guarded” (data source “Guarded” of “Root”): the file's accounts did not admit"),
        "{warnings}"
    );
    assert_eq!(root.open_peer("Gone").err().unwrap().code, Some(100));
    assert_eq!(root.open_peer("Guarded").err().unwrap().code, Some(212));
    assert_eq!(root.open_peer("../Peer").err().unwrap().code, Some(3));
    let peer = root.open_peer("Peer").unwrap();
    let alias = root.open_peer("Alias").unwrap();
    assert_eq!(
        alias.local_path(),
        peer.local_path(),
        "a link is the file it links to"
    );
    let twin = root.open_peer("Twin").err().unwrap();
    assert_eq!(twin.code, Some(802), "{twin:?}");
    assert!(
        twin.message.contains("another file of that name"),
        "{}",
        twin.message
    );
    drop((peer, alias));

    // A reconnect keeps the engines and credentials for later peer opens.
    root.reopen_local(&local(&root_path, Some((ACCOUNT, PASSWORD))))
        .unwrap();
    root.open_peer("Peer").unwrap().close();
    assert_eq!(root.open_peer("Guarded").err().unwrap().code, Some(212));
    root.close();
    assert_eq!(std::fs::read(&root_path).unwrap(), before);
    let mut names = folder.files();
    names.retain(|n| n.ends_with(".omsave"));
    assert!(names.is_empty(), "{names:?}");
}

#[cfg(unix)]
fn related_owner_layout(root: &mut om_hbam::OwnedNode) {
    let mut alias = om_hbam::serial::decode(&om_hbam::slot::uint_segment(PEER_ROWS)).unwrap();
    let mut def = vec![0x10];
    loop {
        let byte = (alias & 0x7f) as u8;
        alias >>= 7;
        def.push(if alias == 0 { byte } else { byte | 0x80 });
        if alias == 0 {
            break;
        }
    }
    root.put(
        &[
            &[4],
            &[5],
            &om_hbam::slot::uint_segment(fixture::NOTES_LAYOUT),
            &[2],
        ],
        om_hbam::OwnedNode::leaf(Vec::new(), def),
    );
}

#[cfg(unix)]
#[test]
fn a_read_only_related_owner_disables_and_refuses_writes() {
    use std::os::unix::fs::PermissionsExt;
    let folder = Folder::new("local-owner-readonly");
    let root_path = folder.file("Root.fmp12", PASSWORD, |root| {
        related::reference(root, (PEER_ROWS, "Peer rows"), 0x8001, "Peer");
        related_owner_layout(root);
    });
    let peer_path = folder.file("Peer.fmp12", PASSWORD, |_| {});
    std::fs::set_permissions(&peer_path, std::fs::Permissions::from_mode(0o444)).unwrap();
    assert!(std::fs::OpenOptions::new()
        .write(true)
        .open(&peer_path)
        .is_err());
    let before = std::fs::read(&peer_path).unwrap();
    let mut root = opened(&root_path);
    let foreign_layout = u32::try_from(fixture::NOTES_LAYOUT).unwrap();
    root.calc_schema().unwrap();
    assert!(
        root.related_file_warnings().is_empty(),
        "{:?}",
        root.related_file_warnings()
    );
    root.records(foreign_layout, 0, 1).unwrap();
    let caps = root.record_capabilities(foreign_layout).unwrap();
    let edit = root.update_record(foreign_layout, 1, name("Must refuse"));
    let create = root.create_record(foreign_layout, name("Must refuse"));
    let delete = root.delete_record(foreign_layout, 1);
    root.close();
    assert_eq!(
        std::fs::read(&peer_path).unwrap(),
        before,
        "peer must stay byte-identical"
    );
    assert!(
        !caps.create && !caps.update && !caps.delete,
        "read-only owner must disable edits"
    );
    assert_eq!(edit.err().unwrap().code, Some(6));
    assert_eq!(create.err().unwrap().code, Some(6));
    assert_eq!(delete.err().unwrap().code, Some(6));
}

#[cfg(unix)]
#[test]
fn imported_file_path_keeps_the_canonical_owners_folder() {
    let folder = Folder::new("local-owner-path-root");
    let elsewhere = Folder::new("local-owner-path-peer");
    let root_path = folder.file("Root.fmp12", PASSWORD, |root| {
        related::reference(root, (PEER_ROWS, "Peer rows"), 0x8001, "Alias");
        related_owner_layout(root);
    });
    let peer_path = elsewhere.file("Peer.fmp12", PASSWORD, |peer| {
        fixture::add_field(peer, TABLE, 19, "Owner Path", 2, 1, 0, 0x2_0020, 0);
        fixture::add_formula(
            peer,
            &fixture::names(),
            (TABLE, fixture::OCCURRENCE),
            19,
            5,
            "Get ( FilePath )",
        );
    });
    std::os::unix::fs::symlink(&peer_path, folder.0.join("Alias.fmp12")).unwrap();
    let mut root = opened(&root_path);
    let foreign_layout = u32::try_from(fixture::NOTES_LAYOUT).unwrap();
    root.calc_schema().unwrap();
    assert!(
        root.related_file_warnings().is_empty(),
        "{:?}",
        root.related_file_warnings()
    );
    let page = root.records(foreign_layout, 0, 1).unwrap();
    let actual = text(&page, 1, "Owner Path").unwrap().to_owned();
    let expected = om_data::file_refs::local_file_path(&std::fs::canonicalize(&peer_path).unwrap());
    let direct = root.open_peer("Alias").unwrap();
    assert_eq!(
        direct.local_path(),
        Some(std::fs::canonicalize(&peer_path).unwrap())
    );
    drop(direct);
    root.close();
    assert_eq!(
        actual, expected,
        "an imported formula must retain its owning canonical file path"
    );
}

/// A local login is tied to the account key and each file's own settings;
/// imported unstored calculations must not inherit the root's lifetime.
#[cfg(unix)]
#[test]
fn authenticated_account_facts_follow_each_local_file() {
    let folder = Folder::new("account-facts");
    let today = om_data::host::session::today(om_provider::local_utc_offset());
    let (year, month, day) = today.ymd();
    let encoded = 0x800000 | ((year as u32) << 10) | (month << 6) | day;
    let stamp = vec![
        5,
        (encoded >> 16) as u8,
        (encoded >> 8) as u8,
        encoded as u8,
        0x80,
        0,
    ];
    let set_lifetime = |tree: &mut om_hbam::OwnedNode, days: u8| {
        for record in [2, 5] {
            tree.put(
                &[&[23], &[1], &[5], &[record], &[0x0a]],
                om_hbam::OwnedNode::leaf(Vec::new(), stamp.clone()),
            );
        }
        tree.put(
            &[&[23], &[2], &[5], &[1], &[0x0c]],
            om_hbam::OwnedNode::leaf(Vec::new(), vec![0, days]),
        );
    };
    let root_path = folder.file("Root.fmp12", PASSWORD, |root| {
        set_lifetime(root, 10);
        related::reference(root, (PEER_ROWS, "Peer rows"), 0x8001, "Peer");
        related_owner_layout(root);
    });
    folder.file("Peer.fmp12", PASSWORD, |peer| {
        set_lifetime(peer, 20);
        fixture::add_field(peer, TABLE, 19, "Account facts", 2, 1, 0, 0x2_0020, 0);
        fixture::add_formula(
            peer,
            &fixture::names(),
            (TABLE, fixture::OCCURRENCE),
            19,
            5,
            "Get ( AccountType ) & \"|\" & Get ( AccountPasswordDaysRemaining )",
        );
    });
    let mut root = opened(&root_path);
    assert_eq!(
        root.account_info().account_type.as_deref(),
        Some("FileMaker File")
    );
    assert_eq!(
        root.account_info().password_expiry,
        Some(today.day_number() + 10)
    );
    root.calc_schema().unwrap();
    let page = root.records(fixture::NOTES_LAYOUT as u32, 0, 1).unwrap();
    assert_eq!(text(&page, 1, "Account facts"), Some("FileMaker File|20"));
    let mut peer = root.open_peer("Peer").unwrap();
    assert_eq!(
        peer.account_info().password_expiry,
        Some(today.day_number() + 20)
    );
    peer.close();
    root.close();
    let mut second = clean_client();
    second
        .open_local(&local(&root_path, Some((NEWCOMER, NEWCOMER_PASSWORD))))
        .unwrap();
    assert_eq!(second.session_account().as_deref(), Some(NEWCOMER));
    assert_eq!(
        second.account_info().password_expiry,
        Some(today.day_number() + 10)
    );
    second.close();
    // A passwordless account returns -1 even with a configured lifetime.
    let empty_path = folder.file("Empty.fmp12", "", |tree| set_lifetime(tree, 10));
    let mut empty = clean_client();
    empty
        .open_local(&local(&empty_path, Some((ACCOUNT, ""))))
        .unwrap();
    assert_eq!(empty.account_info().password_expiry, Some(-1));
    empty.close();
}

/// Index the table and occurrence catalogs `[3, 16]`, `[3, 17]` as
/// FileMaker indexes them (schema-path.md 5.1).
fn index_schema_catalogs(root: &mut om_hbam::OwnedNode) {
    use om_hbam::{serial, OwnedNode};
    for catalog in [0x10u8, 0x11] {
        let members = root.at(&[&[3], &[catalog], &[5]]).cloned().unwrap();
        let mut index = OwnedNode::list(vec![1], Vec::new());
        let mut last = 0;
        for (n, member) in members.children.iter().enumerate() {
            last = last.max(serial::decode(&member.segment).unwrap());
            let name = om_data::field_catalog::member_name(member).unwrap();
            let id = [&[member.segment.len() as u8][..], &member.segment].concat();
            let mut listed = om_data::field_catalog::name_key(&name);
            listed.extend([0, 0, 0]);
            index.put(&[&[1], &listed], OwnedNode::leaf(Vec::new(), id.clone()));
            index.put(
                &[&[3], &(n as u32 + 1).to_be_bytes()],
                OwnedNode::leaf(Vec::new(), id),
            );
        }
        let last = serial::encode(last);
        index.set_child(OwnedNode::leaf(
            vec![0],
            [&[last.len() as u8][..], &last].concat(),
        ));
        index.set_child(OwnedNode::leaf(
            vec![4],
            (members.children.len() as u32).to_be_bytes().to_vec(),
        ));
        root.put(&[&[3], &[catalog], &[1]], index);
    }
}

/// New Layout/Report saves to a file opened from this computer: the new
/// layout is listed at once, and after the file opens again it is there,
/// showing the chosen occurrence's records in Form view with one empty
/// body part, in the theme and width of the layout it was made from; a
/// second layout may take the same name.
#[test]
fn a_new_layout_is_saved_in_a_local_file() {
    use om_data::layout_catalog::{LayoutRequest, BODY_HEIGHT};
    let folder = Folder::new("new-layout");
    let path = folder.file(FILE, PASSWORD, fixture::index_layouts);
    let mut provider = opened(&path);
    let request = |name: &str| LayoutRequest {
        name: name.into(),
        occurrence: fixture::OCCURRENCE,
        like: Some(fixture::LAYOUT as u32),
    };
    let created = provider.create_layout(request("Task Form")).unwrap();
    assert!(created.changed);
    let listed = |p: &mut NativeProvider| -> Vec<(u32, String)> {
        p.layouts()
            .unwrap()
            .into_iter()
            .map(|l| (l.id, l.name))
            .collect()
    };
    assert_eq!(
        listed(&mut provider).last(),
        Some(&(created.layout, "Task Form".into()))
    );
    let again = provider.create_layout(request("Task Form")).unwrap();
    assert_eq!(again.layout, created.layout + 1);
    provider.close();

    let mut reopened = opened(&path);
    let names = listed(&mut reopened);
    assert_eq!(
        &names[names.len() - 2..],
        [
            (created.layout, "Task Form".to_owned()),
            (again.layout, "Task Form".to_owned())
        ]
    );
    let render = reopened
        .render_layout(created.layout)
        .unwrap()
        .expect("the new layout");
    let def = render.layout.def.as_ref().expect("its definition");
    assert_eq!(
        om_layout::occurrence_id(def.table_alias_key),
        Some(fixture::OCCURRENCE)
    );
    assert_eq!(def.flags & 0x4000, 0, "Form view, not Table");
    assert_eq!(def.flags & 0x200, 0, "Form view, not List");
    assert_eq!(render.layout.parts.len(), 1);
    assert_eq!(render.layout.parts[0].height, BODY_HEIGHT);
    assert!(render.layout.objects.objects.is_empty());
    reopened.close();
}

/// Manage Database saves to a file opened from this computer: its own
/// host admits only the file's [Full Access] accounts, so the one logged
/// in may change the schema, and a new table and its occurrence are on
/// disk when the file opens again.
#[test]
fn manage_database_saves_a_new_table_in_a_local_file() {
    use om_data::SchemaChange;
    let folder = Folder::new("schema");
    let path = folder.file(FILE, PASSWORD, index_schema_catalogs);
    let mut provider = opened(&path);
    let schema = provider.begin_schema_edit().unwrap();
    assert!(schema.capabilities.tables, "{:?}", schema.capabilities);
    provider
        .apply_schema_change(SchemaChange::CreateTable {
            name: "Projects".into(),
            comment: "Grouped work".into(),
        })
        .unwrap();
    let saved = provider.commit_schema_changes().unwrap();
    assert!(saved.tables.iter().any(|t| t.name == "Projects"));
    provider.close();

    let mut again = opened(&path);
    let schema = again.database_schema().unwrap();
    let projects = schema
        .tables
        .iter()
        .find(|t| t.name == "Projects")
        .expect("the table on disk");
    assert_eq!(projects.comment, "Grouped work");
    assert!(schema
        .occurrences
        .iter()
        .any(|o| o.name == "Projects" && o.table_id == Some(projects.id)));
    again.close();
}

/// The field catalog's list of fields by type (`[T, 3, 1, 6]`), as a file
/// FileMaker made holds it: a key per field, its kind, its data type times
/// four plus one for a global, its name key and two zero bytes, holding the
/// field's id.
fn list_fields_by_type(root: &mut om_hbam::OwnedNode) {
    use om_hbam::OwnedNode;
    let table = om_hbam::slot::uint_segment(TABLE);
    let members = root.at(&[&table, &[3], &[5]]).cloned().unwrap();
    let mut list = OwnedNode::list(vec![6], Vec::new());
    for member in &members.children {
        let name = om_data::field_catalog::member_name(member).unwrap();
        let definition = member.child(&[2]).and_then(|d| d.value.clone()).unwrap();
        let options = u32::from_be_bytes(definition[8..12].try_into().unwrap());
        let mut key = vec![
            definition[0],
            definition[1] << 2 | u8::from(options & 0x1_0000 != 0),
        ];
        key.extend(om_data::field_catalog::name_key(&name));
        key.extend([0, 0]);
        let id = [&[member.segment.len() as u8][..], &member.segment].concat();
        list.put(&[&key], OwnedNode::leaf(Vec::new(), id));
    }
    root.put(&[&table, &[3], &[1], &[6]], list);
}

/// What a closed file holds of Tasks' field Name: each record's stored
/// cell, the value index's keys and its counts, and the type byte of its
/// key in the list of fields by type.
#[derive(Debug, PartialEq)]
struct NameOnDisk {
    cells: std::collections::BTreeMap<Vec<u8>, Vec<u8>>,
    keys: Vec<Vec<u8>>,
    counts: Option<Vec<u8>>,
    by_type: Vec<u8>,
}

fn name_on_disk(path: &Path) -> NameOnDisk {
    let file = om_hbam::HbamFile::open(path).unwrap();
    let tree = file.key_tree().unwrap();
    let table = om_hbam::slot::uint_segment(TABLE);
    let field = om_hbam::slot::uint_segment(fixture::NAME);
    let cells = tree
        .node(&[&table, &[5]])
        .map(|records| {
            records
                .children
                .iter()
                .filter_map(|r| {
                    Some((r.segment.0.to_vec(), r.child(&field)?.assembled()?.to_vec()))
                })
                .collect()
        })
        .unwrap_or_default();
    let index = tree.node(&[&table, &[1], &field]);
    let keys = index
        .and_then(|i| i.child(&[0]))
        .map(|k| k.children.iter().map(|c| c.segment.0.to_vec()).collect())
        .unwrap_or_default();
    let counts = index
        .and_then(|i| i.child(&[0x10]))
        .and_then(|c| c.assembled())
        .map(|c| c.to_vec());
    let name = om_data::field_catalog::name_key("Name");
    let by_type = tree
        .node(&[&table, &[3], &[1], &[6]])
        .map(|list| {
            list.children
                .iter()
                .filter(|k| k.segment.0.get(2..2 + name.len()) == Some(name.as_slice()))
                .map(|k| k.segment.0[1])
                .collect()
        })
        .unwrap_or_default();
    NameOnDisk {
        cells,
        keys,
        counts,
        by_type,
    }
}

/// Manage Database changes an indexed text field holding values to number
/// and back (#441), as FileMaker Pro 26.0.3 did on the same five values
/// (#441, 2026-11-14): no stored byte changes, the field's key in the list
/// of fields by type moves from type byte `04` to `08` and back, the value
/// index is rebuilt with numeric keys and then the original text keys, and
/// a sort orders numbers, then text. A change to a container is refused
/// first in the same edit and leaves nothing behind.
#[test]
fn a_text_field_with_values_changes_to_number_and_back_as_filemaker_does() {
    use om_data::field_options::Indexing;
    use om_data::found::{FoundSetChange, SortField, SortOrder};
    use om_data::{FieldKind, SchemaChange};
    let folder = Folder::new("type-change");
    let path = folder.file(FILE, PASSWORD, |root| {
        index_schema_catalogs(root);
        list_fields_by_type(root);
    });
    let values = ["10", "2", "-3", "4.5", "N/A"];
    let mut provider = opened(&path);
    let tasks = layout(&mut provider, "Tasks");
    for id in [1, 2] {
        provider.delete_record(tasks, id).unwrap();
    }
    let ids: Vec<i64> = values
        .iter()
        .map(|value| provider.create_record(tasks, name(value)).unwrap().id)
        .collect();
    let field = fixture::NAME;
    // Indexed, as the field FileMaker Pro changed was.
    provider.begin_schema_edit().unwrap();
    let mut options = provider
        .database_schema()
        .unwrap()
        .table(TABLE)
        .and_then(|t| t.fields.iter().find(|f| f.id == field))
        .and_then(|f| f.options.clone())
        .unwrap();
    options.storage.indexing = Indexing::Minimal;
    provider
        .apply_schema_change(SchemaChange::SetFieldOptions {
            table_id: TABLE,
            field_id: field,
            options: Box::new(options),
        })
        .unwrap();
    provider.commit_schema_changes().unwrap();
    provider.close();
    let text = name_on_disk(&path);
    assert_eq!(text.cells.len(), 5);
    assert_eq!(text.keys.len(), 5);
    assert_eq!(text.by_type, [0x04]);

    let change = |data_type| SchemaChange::ChangeField {
        table_id: TABLE,
        field_id: field,
        name: "Name".into(),
        data_type,
        comment: String::new(),
    };
    let retype = |data_type| {
        let mut p = opened(&path);
        p.begin_schema_edit().unwrap();
        let refused = p
            .apply_schema_change(change(FieldKind::Container))
            .unwrap_err();
        assert!(refused.message.contains("to a container"), "{refused:?}");
        p.apply_schema_change(change(data_type)).unwrap();
        p.commit_schema_changes().unwrap();
        p.close();
        name_on_disk(&path)
    };
    let sorted = || {
        let mut p = opened(&path);
        let tasks = layout(&mut p, "Tasks");
        p.change_found_set(tasks, FoundSetChange::ShowAll).unwrap();
        p.change_found_set(
            tasks,
            FoundSetChange::Sort(vec![SortField::new("Name", SortOrder::Ascending)]),
        )
        .unwrap();
        let order: Vec<i64> = p
            .records(tasks, 0, 10)
            .unwrap()
            .records
            .iter()
            .map(|r| r.id)
            .collect();
        p.close();
        order
    };

    let number = retype(FieldKind::Number);
    assert_eq!(number.cells, text.cells, "every stored value byte for byte");
    assert_eq!(number.by_type, [0x08]);
    assert_ne!(number.keys, text.keys, "the value index is keyed anew");
    // Each number's key, and "N/A" under the invalid value's `fe ff`, where
    // FileMaker Pro 26.0.3 listed it in the same change (query-program.md
    // 7.3).
    for value in &values[..4] {
        let key = om_collate::number_key(value).unwrap();
        assert!(number.keys.contains(&key), "{value}");
    }
    assert!(
        number.keys.iter().any(|k| k == om_query::keys::INVALID_KEY),
        "N/A"
    );
    assert_eq!(
        sorted(),
        [ids[2], ids[1], ids[3], ids[0], ids[4]],
        "-3, 2, 4.5, 10, N/A"
    );

    let back = retype(FieldKind::Text);
    assert_eq!(back.cells, text.cells);
    assert_eq!(back.by_type, [0x04]);
    assert_eq!(back.keys, text.keys, "the text keys byte for byte");
    assert_eq!(back.counts, text.counts);
    assert_eq!(
        sorted(),
        [ids[2], ids[0], ids[1], ids[3], ids[4]],
        "-3, 10, 2, 4.5, N/A"
    );
}

/// An index a find built on demand, on a field defined with no index, is
/// keyed anew when the field's type changes (#441): a number find then
/// reads number keys, not the text keys of before.
#[test]
fn an_index_built_for_a_find_is_keyed_anew_when_the_type_changes() {
    use om_data::found::{FindKind, FindRequest, FoundSetChange};
    use om_data::{FieldKind, SchemaChange};
    let find = |p: &mut NativeProvider, criterion: &str| {
        let tasks = layout(p, "Tasks");
        let mut request = FindRequest::default();
        request.set("Name", criterion);
        p.change_found_set(
            tasks,
            FoundSetChange::Find {
                requests: vec![request],
                kind: FindKind::Replace,
            },
        )
        .map(|found| found.found)
    };
    let folder = Folder::new("type-change-index");
    let path = folder.file(FILE, PASSWORD, index_schema_catalogs);
    let mut provider = opened(&path);
    let tasks = layout(&mut provider, "Tasks");
    for value in ["10", "2", "100"] {
        provider.create_record(tasks, name(value)).unwrap();
    }
    // An exact find reads the value index, built for it and kept.
    assert_eq!(find(&mut provider, "==10").unwrap(), 1);
    provider.close();
    let before = name_on_disk(&path).keys;
    assert!(!before.is_empty(), "the index the find built");

    let mut provider = opened(&path);
    provider.begin_schema_edit().unwrap();
    provider
        .apply_schema_change(SchemaChange::ChangeField {
            table_id: TABLE,
            field_id: fixture::NAME,
            name: "Name".into(),
            data_type: FieldKind::Number,
            comment: String::new(),
        })
        .unwrap();
    provider.commit_schema_changes().unwrap();
    provider.close();
    let after = name_on_disk(&path).keys;
    assert!(
        after.contains(&om_collate::number_key("10").unwrap()),
        "{after:?}"
    );

    let mut provider = opened(&path);
    assert_eq!(find(&mut provider, "10").unwrap(), 1, "the number 10 alone");
    provider.close();
}

/// A relationship made in Manage Database is on disk when the file opens
/// again: a second occurrence of Tasks related to the first on Name, with
/// "Allow creation" on the right, its box where it was put.
#[test]
fn manage_database_saves_a_new_relationship_in_a_local_file() {
    use om_data::{RelationshipMatch, RelationshipSide, SchemaChange};
    let folder = Folder::new("relationship");
    let path = folder.file(FILE, PASSWORD, index_schema_catalogs);
    let mut provider = opened(&path);
    provider.begin_schema_edit().unwrap();
    let staged = provider
        .apply_schema_change(SchemaChange::CreateOccurrence {
            table_id: TABLE,
            name: "Tasks 2".into(),
        })
        .unwrap();
    let second = staged
        .occurrences
        .iter()
        .find(|o| o.name == "Tasks 2")
        .unwrap()
        .id;
    let first = staged
        .occurrences
        .iter()
        .find(|o| o.name == "Tasks")
        .unwrap()
        .id;
    for change in [
        SchemaChange::ArrangeOccurrence {
            occurrence_id: second,
            left: 260,
            top: 40,
            width: 131,
            height: 116,
        },
        SchemaChange::CreateRelationship {
            left_occurrence: first,
            right_occurrence: second,
            matches: vec![RelationshipMatch {
                left_field: fixture::NAME,
                operator: 0,
                right_field: fixture::NAME,
            }],
            left: RelationshipSide::default(),
            right: RelationshipSide {
                allow_creation: true,
                ..Default::default()
            },
        },
    ] {
        provider.apply_schema_change(change).unwrap();
    }
    provider.commit_schema_changes().unwrap();
    provider.close();

    let mut again = opened(&path);
    let schema = again.database_schema().unwrap();
    let relationship = schema
        .relationships
        .iter()
        .find(|r| r.left_occurrence == first && r.right_occurrence == second)
        .expect("the relationship on disk");
    assert!(relationship.right().allow_creation && !relationship.left().allow_creation);
    assert_eq!(
        relationship
            .matches
            .iter()
            .map(|m| (m.left_field, m.operator, m.right_field))
            .collect::<Vec<_>>(),
        [(fixture::NAME, 0, fixture::NAME)]
    );
    again.close();
}

/// Calculation fields made through the local-file path (#432), for
/// FileMaker's own tools to open the file afterwards (schema-manager.md,
/// "Validation"): a concatenation, an `IsEmpty` test, a constant and
/// calculations over the related Phone Numbers in the Contacts table of a
/// copy of the Contacts starter solution.
#[test]
#[ignore = "requires OM_CALCULATION_FILE, a writable copy of the Contacts starter solution (Admin, no password)"]
fn calculation_fields_are_made_in_a_copy_of_a_filemaker_file() {
    use om_data::calc_field::CalculationField;
    use om_data::{FieldKind, SchemaChange};
    let path = PathBuf::from(std::env::var_os("OM_CALCULATION_FILE").expect("a file"));
    let mut provider = clean_client();
    provider
        .open_local(&local(&path, Some(("Admin", ""))))
        .unwrap();
    let schema = provider.begin_schema_edit().unwrap();
    let table = schema
        .tables
        .iter()
        .find(|t| t.name == "Contacts")
        .unwrap()
        .id;
    let context = schema
        .occurrences
        .iter()
        .find(|o| o.name == "Contacts")
        .unwrap()
        .id;
    for (name, formula, result) in [
        (
            "Full Name",
            "First Name & \" \" & Last Name",
            FieldKind::Text,
        ),
        (
            "Titled",
            "If ( IsEmpty ( Title ) ; \"none\" ; Title )",
            FieldKind::Text,
        ),
        ("Answer", "42", FieldKind::Number),
        (
            "Phones",
            "Count ( Phone Numbers::Number )",
            FieldKind::Number,
        ),
        (
            "Phone List",
            "List ( Phone Numbers::Number )",
            FieldKind::Text,
        ),
        (
            "First Phone",
            "Phone Numbers::Number & \"!\"",
            FieldKind::Text,
        ),
    ] {
        provider
            .apply_schema_change(SchemaChange::CreateCalculationField {
                table_id: table,
                name: name.into(),
                comment: String::new(),
                calculation: CalculationField {
                    result,
                    ..CalculationField::new(formula, context)
                },
            })
            .unwrap_or_else(|e| panic!("{name}: {}", e.message));
    }
    provider.commit_schema_changes().unwrap();
    provider.close();
}

/// Field options made through the local-file path (#431), for FileMaker's
/// own tools to check afterwards (schema-manager.md, "Validation"): in the
/// Contacts table of a copy of the Contacts starter solution, a new field
/// with a calculated value, Title validated by its value list, Job Title
/// by a calculation with a custom message, and a new number field always
/// validated not empty and in a range.
#[test]
#[ignore = "requires OM_OPTIONS_FILE, a writable copy of the Contacts starter solution (Admin, no password)"]
fn field_options_are_made_in_a_copy_of_a_filemaker_file() {
    use om_data::field_options::{
        AutoCalculation, AutoEnterValue, Message, OptionFormula, ValidationCalculation,
    };
    use om_data::{FieldKind, SchemaChange};
    let path = PathBuf::from(std::env::var_os("OM_OPTIONS_FILE").expect("a file"));
    let mut provider = clean_client();
    provider
        .open_local(&local(&path, Some(("Admin", ""))))
        .unwrap();
    let mut schema = provider.begin_schema_edit().unwrap();
    let table = schema
        .tables
        .iter()
        .find(|t| t.name == "Contacts")
        .unwrap()
        .id;
    let context = schema
        .occurrences
        .iter()
        .find(|o| o.name == "Contacts")
        .unwrap()
        .id;
    for (name, data_type) in [
        ("Full Name", FieldKind::Text),
        ("Rating", FieldKind::Number),
    ] {
        schema = provider
            .apply_schema_change(SchemaChange::CreateField {
                table_id: table,
                name: name.into(),
                data_type,
                comment: String::new(),
            })
            .unwrap_or_else(|e| panic!("{name}: {}", e.message));
    }
    let contacts = schema.table(table).unwrap();
    let field = |name: &str| {
        contacts
            .fields
            .iter()
            .find(|f| f.name == name)
            .cloned()
            .unwrap_or_else(|| panic!("{name}"))
    };
    let mut changes = Vec::new();
    let mut full = field("Full Name");
    let options = full.options.as_mut().unwrap();
    options.auto_enter.value = AutoEnterValue::Calculated(AutoCalculation::new(
        OptionFormula::new("First Name & \" \" & Last Name", context),
    ));
    changes.push(full);
    let mut title = field("Title");
    title.options.as_mut().unwrap().validation.value_list = Some(3);
    changes.push(title);
    let mut job = field("Job Title");
    let rules = &mut job.options.as_mut().unwrap().validation;
    rules.calculation = Some(ValidationCalculation {
        formula: OptionFormula::new("Length ( Self ) < 100", context),
        only_if_modified: true,
    });
    rules.message = Some(Message::Text("At most 99 characters.".into()));
    changes.push(job);
    let mut rating = field("Rating");
    let rules = &mut rating.options.as_mut().unwrap().validation;
    rules.always = true;
    rules.not_empty = true;
    rules.range = Some(("1".into(), "5".into()));
    changes.push(rating);
    for changed in changes {
        provider
            .apply_schema_change(SchemaChange::SetFieldOptions {
                table_id: table,
                field_id: changed.id,
                options: Box::new(changed.options.unwrap()),
            })
            .unwrap_or_else(|e| panic!("{}: {}", changed.name, e.message));
    }
    provider.commit_schema_changes().unwrap();
    provider.close();
}

/// Lookups through the local-file path (#431), for FileMaker's own tools
/// to check afterwards (schema-manager.md, "Validation"): in a copy of the
/// Contacts starter solution, a new Phone Numbers field looks up
/// Contacts::Company; in a copy of the Assets starter solution, whose
/// Assignments table holds the templates' only lookup, Assignments::Note
/// gets a creation value, which rebuilds that table's evaluation lists
/// around the lookup.
#[test]
#[ignore = "requires OM_LOOKUP_FILE and OM_ASSETS_FILE, writable copies of the Contacts and Assets starter solutions (Admin, no password)"]
fn lookups_are_made_in_copies_of_filemaker_files() {
    use om_data::field_options::{AutoEnterValue, EntryValue, Lookup, NoMatch};
    use om_data::{FieldKind, SchemaChange};
    let open = |variable: &str| {
        let path = PathBuf::from(std::env::var_os(variable).expect("a file"));
        let mut provider = clean_client();
        provider
            .open_local(&local(&path, Some(("Admin", ""))))
            .unwrap();
        let schema = provider.begin_schema_edit().unwrap();
        (provider, schema)
    };
    let occurrence = |schema: &om_data::DatabaseSchema, name: &str| {
        schema
            .occurrences
            .iter()
            .find(|o| o.name == name)
            .unwrap()
            .id
    };
    let table = |schema: &om_data::DatabaseSchema, name: &str| {
        schema
            .tables
            .iter()
            .find(|t| t.name == name)
            .unwrap()
            .clone()
    };

    let (mut provider, schema) = open("OM_LOOKUP_FILE");
    let phones = table(&schema, "Phone Numbers");
    let company = table(&schema, "Contacts")
        .fields
        .iter()
        .find(|f| f.name == "Company")
        .unwrap()
        .id;
    let schema = provider
        .apply_schema_change(SchemaChange::CreateField {
            table_id: phones.id,
            name: "Contact Company".into(),
            data_type: FieldKind::Text,
            comment: String::new(),
        })
        .unwrap();
    let field = table(&schema, "Phone Numbers")
        .fields
        .iter()
        .find(|f| f.name == "Contact Company")
        .cloned()
        .unwrap();
    let mut options = field.options.unwrap();
    options.auto_enter.value = AutoEnterValue::LookedUp(Some(Lookup {
        from: occurrence(&schema, "Phone Numbers"),
        to: occurrence(&schema, "Contacts"),
        copied: company,
        copy_empty: false,
        no_match: NoMatch::DoNotCopy,
    }));
    provider
        .apply_schema_change(SchemaChange::SetFieldOptions {
            table_id: phones.id,
            field_id: field.id,
            options: Box::new(options),
        })
        .unwrap_or_else(|e| panic!("the lookup: {}", e.message));
    provider.commit_schema_changes().unwrap();
    provider.close();

    let (mut provider, schema) = open("OM_ASSETS_FILE");
    let assignments = table(&schema, "Assignments");
    let note = assignments
        .fields
        .iter()
        .find(|f| f.name == "Note")
        .cloned()
        .unwrap();
    let mut options = note.options.unwrap();
    options.auto_enter.value = AutoEnterValue::Creation(EntryValue::Name);
    provider
        .apply_schema_change(SchemaChange::SetFieldOptions {
            table_id: assignments.id,
            field_id: note.id,
            options: Box::new(options),
        })
        .unwrap_or_else(|e| panic!("Assignments::Note: {}", e.message));
    provider.commit_schema_changes().unwrap();
    provider.close();
}
/// Global storage turned on for an existing field through the local-file
/// path (#431), for FileMaker's own tools to check afterwards
/// (schema-manager.md, "Validation"): Contacts::Website in a copy of the
/// Contacts starter solution, with the deferred work the host then does.
#[test]
#[ignore = "requires OM_GLOBAL_FILE, a writable copy of the Contacts starter solution (Admin, no password)"]
fn global_storage_is_changed_in_a_copy_of_a_filemaker_file() {
    use om_data::field_options::Indexing;
    use om_data::SchemaChange;
    let path = PathBuf::from(std::env::var_os("OM_GLOBAL_FILE").expect("a file"));
    let mut provider = clean_client();
    provider
        .open_local(&local(&path, Some(("Admin", ""))))
        .unwrap();
    let schema = provider.begin_schema_edit().unwrap();
    let contacts = schema.tables.iter().find(|t| t.name == "Contacts").unwrap();
    let website = contacts
        .fields
        .iter()
        .find(|f| f.name == "Website")
        .cloned()
        .unwrap();
    let mut options = website.options.unwrap();
    options.storage.global = true;
    options.storage.indexing = Indexing::None;
    provider
        .apply_schema_change(SchemaChange::SetFieldOptions {
            table_id: contacts.id,
            field_id: website.id,
            options: Box::new(options),
        })
        .unwrap_or_else(|e| panic!("Website: {}", e.message));
    let saved = provider.commit_schema_changes().unwrap();
    let website = saved
        .tables
        .iter()
        .find(|t| t.name == "Contacts")
        .and_then(|t| t.fields.iter().find(|f| f.name == "Website"))
        .and_then(|f| f.options.clone())
        .unwrap();
    assert!(website.storage.global);
    provider.close();
}

/// Formulas over related fields through the local-file path (#431), for
/// FileMaker's own tools to check afterwards (schema-manager.md,
/// "Validation"): in a copy of the Contacts starter solution, a new Phone
/// Numbers field auto-enters `Contacts::Company` and another is validated
/// by `Contacts::Company ≠ ""`, both from Phone Numbers.
#[test]
#[ignore = "requires OM_RELATED_FORMULA_FILE, a writable copy of the Contacts starter solution (Admin, no password)"]
fn related_formulas_are_made_in_a_copy_of_a_filemaker_file() {
    use om_data::field_options::{
        AutoCalculation, AutoEnterValue, OptionFormula, ValidationCalculation,
    };
    use om_data::{FieldKind, SchemaChange};
    let path = PathBuf::from(std::env::var_os("OM_RELATED_FORMULA_FILE").expect("a file"));
    let mut provider = clean_client();
    provider
        .open_local(&local(&path, Some(("Admin", ""))))
        .unwrap();
    let mut schema = provider.begin_schema_edit().unwrap();
    let table = schema
        .tables
        .iter()
        .find(|t| t.name == "Phone Numbers")
        .unwrap()
        .id;
    let context = schema
        .occurrences
        .iter()
        .find(|o| o.name == "Phone Numbers")
        .unwrap()
        .id;
    for name in ["Company Copy", "Needs Company"] {
        schema = provider
            .apply_schema_change(SchemaChange::CreateField {
                table_id: table,
                name: name.into(),
                data_type: FieldKind::Text,
                comment: String::new(),
            })
            .unwrap();
    }
    let field = |name: &str| {
        schema
            .table(table)
            .unwrap()
            .fields
            .iter()
            .find(|f| f.name == name)
            .cloned()
            .unwrap()
    };
    let (copy, needs) = (field("Company Copy"), field("Needs Company"));
    let mut options = copy.options.clone().unwrap();
    options.auto_enter.value = AutoEnterValue::Calculated(AutoCalculation::new(
        OptionFormula::new("Contacts::Company", context),
    ));
    provider
        .apply_schema_change(SchemaChange::SetFieldOptions {
            table_id: table,
            field_id: copy.id,
            options: Box::new(options),
        })
        .unwrap_or_else(|e| panic!("Company Copy: {}", e.message));
    let mut options = needs.options.clone().unwrap();
    options.validation.calculation = Some(ValidationCalculation {
        formula: OptionFormula::new("Contacts::Company ≠ \"\"", context),
        only_if_modified: true,
    });
    provider
        .apply_schema_change(SchemaChange::SetFieldOptions {
            table_id: table,
            field_id: needs.id,
            options: Box::new(options),
        })
        .unwrap_or_else(|e| panic!("Needs Company: {}", e.message));
    provider.commit_schema_changes().unwrap();
    provider.close();
}

/// Occurrences and relationships made through the local-file path (#430),
/// for FileMaker's own tools to check afterwards (schema-manager.md,
/// "Validation"): in a copy of the Contacts starter solution, a second
/// occurrence of Phone Numbers related to Contacts on two pairs with
/// creation and a sort on its side, the Addresses relationship sorted by
/// City descending with deletion, and the Email Addresses relationship
/// deleted.
#[test]
#[ignore = "requires OM_RELATIONSHIP_FILE, a writable copy of the Contacts starter solution (Admin, no password)"]
fn relationships_are_made_in_a_copy_of_a_filemaker_file() {
    use om_data::scripts::specs::{SortEntry, SortSpec, SpecField};
    use om_data::{RelationshipMatch, RelationshipSide, SchemaChange};
    let path = PathBuf::from(std::env::var_os("OM_RELATIONSHIP_FILE").expect("a file"));
    let mut provider = clean_client();
    provider
        .open_local(&local(&path, Some(("Admin", ""))))
        .unwrap();
    let schema = provider.begin_schema_edit().unwrap();
    let occurrence = |schema: &om_data::DatabaseSchema, name: &str| {
        schema
            .occurrences
            .iter()
            .find(|o| o.name == name)
            .unwrap_or_else(|| panic!("{name}"))
            .clone()
    };
    let table = |name: &str| schema.tables.iter().find(|t| t.name == name).unwrap();
    let field = |table: &str, name: &str| {
        let table = schema.tables.iter().find(|t| t.name == table).unwrap();
        table.fields.iter().find(|f| f.name == name).unwrap().id
    };
    let contacts = occurrence(&schema, "Contacts").id;
    let addresses = occurrence(&schema, "Addresses").id;
    let email = occurrence(&schema, "Email Addresses").id;
    let staged = provider
        .apply_schema_change(SchemaChange::CreateOccurrence {
            table_id: table("Phone Numbers").id,
            name: "Phone Numbers 2".into(),
        })
        .unwrap();
    let phones = occurrence(&staged, "Phone Numbers 2").id;
    let order = |occurrence: u64, field: u64, descending: bool| SortSpec {
        keep_sorted: true,
        blanks_last: false,
        fields: vec![SortEntry {
            field: SpecField {
                occurrence: Some(occurrence),
                field,
                repetition: 1,
            },
            summary: None,
            value_list: None,
            descending,
            locale: 0,
        }],
    };
    let relationship = |schema: &om_data::DatabaseSchema, right: u64| {
        schema
            .relationships
            .iter()
            .find(|r| r.left_occurrence == contacts && r.right_occurrence == right)
            .unwrap()
            .clone()
    };
    let to_addresses = relationship(&staged, addresses);
    let to_email = relationship(&staged, email);
    for change in [
        SchemaChange::CreateRelationship {
            left_occurrence: contacts,
            right_occurrence: phones,
            matches: vec![
                RelationshipMatch {
                    left_field: field("Contacts", "PrimaryKey"),
                    operator: 0,
                    right_field: field("Phone Numbers", "ForeignKey"),
                },
                RelationshipMatch {
                    left_field: field("Contacts", "Title"),
                    operator: 1,
                    right_field: field("Phone Numbers", "Type"),
                },
            ],
            left: RelationshipSide::default(),
            right: RelationshipSide {
                allow_creation: true,
                sort: true,
                order: Some(order(phones, field("Phone Numbers", "Number"), false)),
                ..Default::default()
            },
        },
        SchemaChange::ChangeRelationship {
            relationship_id: to_addresses.id,
            matches: to_addresses
                .matches
                .iter()
                .map(|m| RelationshipMatch {
                    left_field: m.left_field,
                    operator: m.operator,
                    right_field: m.right_field,
                })
                .collect(),
            left: to_addresses.left(),
            right: RelationshipSide {
                allow_creation: true,
                delete_related: true,
                sort: true,
                order: Some(order(addresses, field("Addresses", "City"), true)),
            },
        },
        SchemaChange::DeleteRelationship {
            relationship_id: to_email.id,
        },
    ] {
        provider
            .apply_schema_change(change)
            .unwrap_or_else(|e| panic!("{}", e.message));
    }
    provider.commit_schema_changes().unwrap();
    provider.close();
}

/// Duplicate through the local-file path (schema-path.md 5.14), for
/// FileMaker's own tools to check afterwards (schema-manager.md,
/// "Duplicate"): in each of `OM_DUPLICATE_FILES` (`:`-separated writable
/// copies of starter solutions, Admin without a password), every field of
/// every table is duplicated once, but the ones OpenMaker refuses, and the
/// edit is saved.
#[test]
#[ignore = "requires OM_DUPLICATE_FILES, writable copies of starter solutions (Admin, no password)"]
fn fields_are_duplicated_in_copies_of_filemaker_files() {
    use om_data::SchemaChange;
    let files = std::env::var_os("OM_DUPLICATE_FILES").expect("files");
    for path in std::env::split_paths(&files) {
        let at = path.display().to_string();
        let mut provider = clean_client();
        provider
            .open_local(&local(&path, Some(("Admin", ""))))
            .unwrap_or_else(|e| panic!("{at}: {}", e.message));
        let schema = provider.begin_schema_edit().unwrap();
        let (mut copied, mut refused) = (0, Vec::new());
        for table in &schema.tables {
            for field in &table.fields {
                match provider.apply_schema_change(SchemaChange::DuplicateField {
                    table_id: table.id,
                    field_id: field.id,
                }) {
                    Ok(_) => copied += 1,
                    Err(e) => {
                        refused.push(format!("{}::{}: {}", table.name, field.name, e.message))
                    }
                }
            }
        }
        println!("{at}: {copied} copied, refused {refused:#?}");
        provider.commit_schema_changes().unwrap();
        provider.close();
    }
}

/// The shipped starter solutions (`DynamicTemplates/<solution>/<lang>.fmp12`)
/// in this checkout's `re/extracted` or `OM_TEMPLATES`.
fn starter_files() -> Vec<PathBuf> {
    let dir = std::env::var_os("OM_TEMPLATES").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(
            "../../re/extracted/mac/FileMaker Pro 26/FileMaker Pro.app/Contents/Resources/DynamicTemplates",
        )
    });
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .flatten()
        .filter(|e| e.path().is_dir())
        .flat_map(|e| std::fs::read_dir(e.path()).into_iter().flatten().flatten())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "fmp12"))
        .collect();
    files.sort();
    files
}

/// Duplicate (schema-path.md 5.14) on every field of every shipped starter
/// solution, through the local-file path, staged and discarded: each field
/// is copied as FileMaker Pro's Duplicate copies it, or refused with error
/// 3 and the reason. Every field of every language's file is copied, the
/// one lookup (Assignments::Name in Assets) with its template padding
/// (#432). Prints any refusals by reason.
#[test]
#[ignore = "requires the FileMaker Pro 26 starter solutions in re/extracted or OM_TEMPLATES"]
fn every_starter_field_is_duplicated_or_refused() {
    use om_data::SchemaChange;
    let folder = Folder::new("duplicate-survey");
    let mut copied = 0;
    let mut refused: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    let mut english_refused = Vec::new();
    // The lookup this survey is for: an empty starter folder must not
    // pass it (#933's review).
    let mut assets_lookup_copied = false;
    for template in starter_files() {
        let at = template.display().to_string();
        let english = template.file_name().is_some_and(|n| n == "en.fmp12");
        let path = folder.0.join("starter.fmp12");
        std::fs::copy(&template, &path).unwrap();
        let mut provider = clean_client();
        provider
            .open_local(&local(&path, Some(("Admin", ""))))
            .unwrap_or_else(|e| panic!("{at}: {}", e.message));
        let schema = provider.begin_schema_edit().unwrap();
        for table in &schema.tables {
            for field in &table.fields {
                let which = format!("{at}: {}::{}", table.name, field.name);
                match provider.apply_schema_change(SchemaChange::DuplicateField {
                    table_id: table.id,
                    field_id: field.id,
                }) {
                    Ok(_) => {
                        copied += 1;
                        assets_lookup_copied |= english
                            && at.contains("Assets")
                            && table.name == "Assignments"
                            && field.name == "Name";
                    }
                    Err(e) => {
                        assert_eq!(e.code, Some(3), "{which}: {}", e.message);
                        if english {
                            english_refused.push(format!("{which}: {}", e.message));
                        }
                        let reason = e.message.replace(&field.name, "<field>");
                        refused.entry(reason).or_default().push(which);
                    }
                }
            }
        }
        provider.cancel_schema_changes().unwrap();
        provider.close();
    }
    let total: usize = refused.values().map(Vec::len).sum();
    println!("{copied} copied, {total} refused");
    for (reason, fields) in &refused {
        println!("{:5} {reason}\n      e.g. {}", fields.len(), fields[0]);
    }
    assert!(english_refused.is_empty(), "{english_refused:#?}");
    assert_eq!(total, 0, "{refused:#?}");
    assert!(copied > 0, "no starter solution was found");
    assert!(
        assets_lookup_copied,
        "the English Assets file's lookup Assignments::Name was not copied"
    );
}

/// The #137 fixture, kept out of git: FileMaker Pro 26.0.3's English
/// Contacts starter with two records and five custom sets, each a duplicate
/// of [Data Entry Only] with one change, and one account per set (password
/// the account's name). Its path is `OM_CUSTOM_SETS_FIXTURE`, else
/// `re/work/137-custom-sets/Contacts-custom.fmp12`. FileMaker Pro's own
/// behavior under these accounts was not measured.
fn custom_sets_fixture() -> PathBuf {
    std::env::var_os("OM_CUSTOM_SETS_FIXTURE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../re/work/137-custom-sets/Contacts-custom.fmp12")
        })
}

#[test]
#[ignore = "requires the FileMaker-made fixture of #137 (OM_CUSTOM_SETS_FIXTURE, or re/work/137-custom-sets/Contacts-custom.fmp12; not in git)"]
fn a_custom_sets_view_only_field_is_not_changed_and_every_set_opens() {
    let folder = Folder::new("custom-sets");
    let path = folder.0.join("Contacts-custom.fmp12");
    std::fs::copy(custom_sets_fixture(), &path).unwrap();
    // `viewonly`: Contacts::Last Name view only, all else as [Data Entry
    // Only]. Browse does not let the user change it (typing is refused with
    // its alert, #690), and a write to it is refused (201); First Name is
    // written.
    let mut provider = clean_client();
    assert_eq!(
        provider
            .open_local(&local(&path, Some(("viewonly", "viewonly"))))
            .unwrap(),
        LocalOpen::Opened { read_only: None }
    );
    let details = layout(&mut provider, "Contact Details");
    let capabilities = provider.record_capabilities(details).unwrap();
    let editable = capabilities.editable_fields.unwrap();
    assert!(editable.contains("First Name") && !editable.contains("Last Name"));
    assert_eq!(
        capabilities.refused,
        [("Last Name".into(), om_data::EditRefusal::ViewOnlyField)].into()
    );
    let id = provider.records(details, 0, 10).unwrap().records[0].id;
    let change = |field: &str| FieldChanges::from([(field.to_owned(), Some("Changed".into()))]);
    let refused = provider
        .update_record(details, id, change("Last Name"))
        .unwrap_err();
    assert_eq!(refused.code, Some(201), "{refused:?}");
    provider
        .update_record(details, id, change("First Name"))
        .unwrap();
    // `Get ( RecordAccess )` judges the record without moving window 1's
    // pending edit off the version it showed: window 2's change still
    // refuses that edit's save (306).
    let shown = provider.records(details, 0, 10).unwrap();
    provider.copy_window(1, 2);
    provider.retain_record(details, id, shown.versions.get(&id).copied());
    provider.use_window(2);
    provider.records(details, 0, 10).unwrap();
    let theirs = FieldChanges::from([("First Name".to_owned(), Some("Theirs".into()))]);
    provider.update_record(details, id, theirs).unwrap();
    provider.use_window(1);
    assert_eq!(provider.record_access(details, id), Some(2));
    let refused = provider
        .update_record(details, id, change("First Name"))
        .unwrap_err();
    assert_eq!(refused.code, Some(306), "{refused:?}");
    provider.close();
    // `noscript` changes nothing its base does not, and `editonly`'s menus
    // (Editing only) are the desktop's to apply: both open.
    for account in ["noscript", "editonly"] {
        let mut provider = clean_client();
        provider
            .open_local(&local(&path, Some((account, account))))
            .unwrap();
        provider.close();
    }
}

#[test]
#[ignore = "requires the FileMaker-made fixture of #137 (OM_CUSTOM_SETS_FIXTURE, or re/work/137-custom-sets/Contacts-custom.fmp12; not in git)"]
fn a_custom_sets_layout_without_access_is_left_out_and_shows_no_values() {
    let folder = Folder::new("nolayout-set");
    let path = folder.0.join("Contacts-custom.fmp12");
    std::fs::copy(custom_sets_fixture(), &path).unwrap();
    // `nolayout`: layout Contact List no access, all else as [Data Entry
    // Only]. The layout stays in the file's list, which scripts and Get (
    // LayoutNumber ) count, but the layout menu leaves it out and nothing
    // is shown or read through it (9).
    let mut provider = clean_client();
    assert_eq!(
        provider
            .open_local(&local(&path, Some(("nolayout", "nolayout"))))
            .unwrap(),
        LocalOpen::Opened { read_only: None }
    );
    let list = layout(&mut provider, "Contact List");
    let details = layout(&mut provider, "Contact Details");
    let menu = provider.layout_menu().unwrap();
    assert!(!menu.iter().any(|entry| entry.id == list), "{menu:?}");
    assert!(menu.iter().any(|entry| entry.id == details), "{menu:?}");
    assert_eq!(provider.render_layout(list).unwrap_err().code, Some(9));
    // Its found set is read through it, ids only and no value: a script's
    // New Window or Go to Related Record takes a window there (#696).
    let page = provider.records(list, 0, 10).unwrap();
    assert_eq!(page.records.len(), 2);
    assert!(page.fields.is_empty() && page.records.iter().all(|r| r.values.is_empty()));
    assert_eq!(provider.records(details, 0, 10).unwrap().records.len(), 2);
    // Design functions read nothing of it: the calculation schema keeps its
    // name, not its fields or objects.
    let schema = provider.calc_schema().unwrap().unwrap();
    let entry = |id| schema.layouts().iter().find(|l| l.id == id).unwrap();
    assert!(entry(list).fields.is_empty() && entry(list).layout.is_none());
    assert!(!entry(details).fields.is_empty() && entry(details).layout.is_some());
    // Nor do the raw layout reads agents use.
    let refused = provider.layout_member(u64::from(list)).unwrap_err();
    assert_eq!(refused.code, Some(9), "{refused:?}");
    let members = provider.layout_members().unwrap();
    assert!(!members.contains_key(&u64::from(list)));
    assert!(members.contains_key(&u64::from(details)));
}

#[test]
#[ignore = "requires the FileMaker-made fixture of #137 (OM_CUSTOM_SETS_FIXTURE, or re/work/137-custom-sets/Contacts-custom.fmp12; not in git)"]
fn a_custom_sets_view_and_edit_limits_apply_to_each_record() {
    let folder = Folder::new("limited-set");
    let path = folder.0.join("Contacts-custom.fmp12");
    std::fs::copy(custom_sets_fixture(), &path).unwrap();
    // `limited`: Contacts' View and Edit limited by First Name = "Allowed";
    // the file's two records are "Allowed" and "Denied".
    let mut provider = clean_client();
    assert_eq!(
        provider
            .open_local(&local(&path, Some(("limited", "limited"))))
            .unwrap(),
        LocalOpen::Opened { read_only: None }
    );
    let details = layout(&mut provider, "Contact Details");
    let page = provider.records(details, 0, 10).unwrap();
    // Both records are listed; the one the view formula rejects has its
    // values withheld.
    assert_eq!(page.records.len(), 2);
    let names: Vec<Option<&str>> = page
        .records
        .iter()
        .map(|record| text(&page, record.id, "First Name"))
        .collect();
    assert!(names.contains(&Some("Allowed")), "{names:?}");
    assert!(names.contains(&None), "{names:?}");
    let allowed = page
        .records
        .iter()
        .find(|record| text(&page, record.id, "First Name") == Some("Allowed"))
        .unwrap()
        .id;
    let denied = page.records.iter().find(|r| r.id != allowed).unwrap().id;
    let change = FieldChanges::from([("Last Name".to_owned(), Some("Changed".into()))]);
    provider
        .update_record(details, allowed, change.clone())
        .unwrap();
    let refused = provider.update_record(details, denied, change).unwrap_err();
    assert_eq!(refused.code, Some(200), "{refused:?}");
    // Finds read the withheld record without its values: it never
    // matches, and the viewable one still does (the table serves no index).
    let find = |provider: &mut NativeProvider, name: &str| {
        let mut request = om_data::found::FindRequest::default();
        request.set("First Name", name);
        provider
            .change_found_set(
                details,
                om_data::found::FoundSetChange::Find {
                    requests: vec![request],
                    kind: om_data::found::FindKind::Replace,
                },
            )
            .map(|_| {
                let page = provider.records(details, 0, 10).unwrap();
                page.records.iter().map(|r| r.id).collect::<Vec<_>>()
            })
            .map_err(|e| e.code)
    };
    assert_eq!(find(&mut provider, "Allowed"), Ok(vec![allowed]));
    assert_eq!(find(&mut provider, "Denied"), Err(Some(401)));
    provider.close();
}

/// A lookup from a parent table through the local-file path (#657), for
/// FileMaker's own tools to check afterwards (record-writes.md, Lookups):
/// in a copy of the Contacts starter solution, a new Contacts field looks
/// up Phone Numbers::Number from Contacts, keyed on Contacts' PrimaryKey,
/// an auto-entered `Get ( UUID )`.
#[test]
#[ignore = "requires OM_PARENT_LOOKUP_FILE, a writable copy of the Contacts starter solution (Admin, no password)"]
fn parent_lookups_are_made_in_a_copy_of_a_filemaker_file() {
    make_lookup(
        "OM_PARENT_LOOKUP_FILE",
        ("Contacts", "First Phone", "Contact Details"),
        ("Contacts", "Phone Numbers"),
        ("Number", om_data::FieldKind::Text),
    );
}

/// A lookup through a sorted relationship, as [`parent_lookups_are_made_in_a_copy_of_a_filemaker_file`]
/// (auto-entry.md, Lookups, battery s42): in a copy of the Assets starter
/// solution, a new Assets field looks up Assignments::Note from Assets,
/// through the starter's relationship sorted by Assignments::Sort By.
#[test]
#[ignore = "requires OM_SORTED_LOOKUP_FILE, a writable copy of the Assets starter solution (Admin, no password)"]
fn sorted_lookups_are_made_in_a_copy_of_a_filemaker_file() {
    make_lookup(
        "OM_SORTED_LOOKUP_FILE",
        ("Assets", "Last Note", "Asset Details"),
        ("Assets", "Assignments"),
        ("Note", om_data::FieldKind::Text),
    );
}

/// A lookup of another type than text, as
/// [`sorted_lookups_are_made_in_a_copy_of_a_filemaker_file`] (auto-entry.md,
/// Lookups, battery s43): in a copy of the Assets starter solution, a new
/// Assets date field looks up Assignments::Date Returned from Assets.
#[test]
#[ignore = "requires OM_TYPED_LOOKUP_FILE, a writable copy of the Assets starter solution (Admin, no password)"]
fn typed_lookups_are_made_in_a_copy_of_a_filemaker_file() {
    make_lookup(
        "OM_TYPED_LOOKUP_FILE",
        ("Assets", "Last Returned", "Asset Details"),
        ("Assets", "Assignments"),
        ("Date Returned", om_data::FieldKind::Date),
    );
}

/// In the file named by environment variable `file`, a new field `name` of
/// `table` of type `data_type` looking up field `copied` of occurrence `to`
/// from occurrence `from` ("do not copy" without a match), made through
/// Create and Options for Field and saved; the record rules then still
/// write `table` on its layout `details`.
fn make_lookup(
    file: &str,
    (table, name, details): (&str, &str, &str),
    (from, to): (&str, &str),
    (copied, data_type): (&str, om_data::FieldKind),
) {
    use om_data::field_options::{AutoEnterValue, Lookup, NoMatch};
    use om_data::SchemaChange;
    let path = PathBuf::from(std::env::var_os(file).expect("a file"));
    let mut provider = clean_client();
    provider
        .open_local(&local(&path, Some(("Admin", ""))))
        .unwrap();
    let schema = provider.begin_schema_edit().unwrap();
    let occurrence = |name: &str| {
        schema
            .occurrences
            .iter()
            .find(|o| o.name == name)
            .unwrap()
            .clone()
    };
    let (from, to) = (occurrence(from), occurrence(to));
    let table_of = |schema: &om_data::DatabaseSchema, id: u64| {
        schema.tables.iter().find(|t| t.id == id).unwrap().clone()
    };
    let field_of = |table: &om_data::SchemaTable, name: &str| {
        table
            .fields
            .iter()
            .find(|f| f.name == name)
            .cloned()
            .unwrap()
    };
    let starting = table_of(&schema, from.table_id.unwrap());
    assert_eq!(starting.name, table);
    let copied = field_of(&table_of(&schema, to.table_id.unwrap()), copied).id;
    let schema = provider
        .apply_schema_change(SchemaChange::CreateField {
            table_id: starting.id,
            name: name.into(),
            data_type,
            comment: String::new(),
        })
        .unwrap();
    let field = field_of(&table_of(&schema, starting.id), name);
    let mut options = field.options.unwrap();
    options.auto_enter.value = AutoEnterValue::LookedUp(Some(Lookup {
        from: from.id,
        to: to.id,
        copied,
        copy_empty: true,
        no_match: NoMatch::DoNotCopy,
    }));
    provider
        .apply_schema_change(SchemaChange::SetFieldOptions {
            table_id: starting.id,
            field_id: field.id,
            options: Box::new(options),
        })
        .unwrap_or_else(|e| panic!("the lookup: {}", e.message));
    provider.commit_schema_changes().unwrap();
    let details = layout(&mut provider, details);
    assert!(provider.record_capabilities(details).unwrap().update);
    provider.close();
}

/// The Fields tab's Options text (client-ui.md, "Manage Database: the
/// Fields tab's third column") for every field of the six English starter
/// solutions in this checkout's `re/extracted` or `OM_TEMPLATES`, printed,
/// with a primary key, a calculation and a summary pinned.
#[test]
#[ignore = "requires the FileMaker Pro 26 starter solutions in re/extracted or OM_TEMPLATES"]
fn starter_fields_read_as_the_fields_tab_lists_them() {
    use om_data::FieldColumn;
    let dir = std::env::var_os("OM_TEMPLATES").map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from(
            "../../re/extracted/mac/FileMaker Pro 26/FileMaker Pro.app/Contents/Resources/DynamicTemplates",
        )
    });
    let folder = Folder::new("options-text");
    let mut texts = std::collections::BTreeMap::new();
    for solution in [
        "Assets",
        "Contacts",
        "Content_Management",
        "Inventory",
        "Meetings",
        "Tasks",
    ] {
        let path = folder.0.join(format!("{solution}.fmp12"));
        std::fs::copy(dir.join(solution).join("en.fmp12"), &path).unwrap();
        let mut provider = clean_client();
        provider
            .open_local(&local(&path, Some(("Admin", ""))))
            .unwrap_or_else(|e| panic!("{solution}: {}", e.message));
        let schema = provider.database_schema().unwrap();
        for table in &schema.tables {
            for field in &table.fields {
                let text = schema.field_column_text(table, field, FieldColumn::Options);
                println!("{solution}\t{}\t{}\t{text}", table.name, field.name);
                texts.insert((solution, table.name.clone(), field.name.clone()), text);
            }
        }
        provider.close();
    }
    let text = |solution: &'static str, table: &str, field: &str| {
        texts[&(solution, table.to_owned(), field.to_owned())].clone()
    };
    assert_eq!(
        text("Contacts", "Contacts", "PrimaryKey"),
        "Indexed, Auto-enter Calculation replaces existing value, Can't Modify Auto, \
         Required Value, Unique"
    );
    assert!(text("Contacts", "Contacts", "Initial").contains("= "));
    assert!(texts.values().any(|t| t.starts_with("= Total of ")));
}

#[test]
fn unsupported_stored_record_position_does_not_reserve_a_serial() {
    use fixture::{LAYOUT, NAME, OCCURRENCE};
    use om_data::rules::{error, RecordNumber};
    use om_hbam::slot::uint_segment;

    const SERIAL: u64 = 40;
    const POSITION: u64 = 41;
    let folder = Folder::new("stored-position-preflight");
    let path = folder.file("Events.fmp12", PASSWORD, |root| {
        fixture::add_field(root, TABLE, SERIAL, "Serial", 1, 1, 0, 0x2, 0);
        fixture::set_field_serial_increment(root, TABLE, SERIAL, 1);
        root.put(
            &[
                &uint_segment(TABLE),
                &[3],
                &[5],
                &uint_segment(SERIAL),
                &[0x0c],
            ],
            om_scripts::text_node(&[], "1"),
        );
        fixture::add_field(root, TABLE, POSITION, "Position", 2, 2, 0, 0x80000, 0);
        fixture::add_formula(
            root,
            &fixture::names(),
            (TABLE, OCCURRENCE),
            POSITION,
            5,
            "Get ( RecordNumber )",
        );
    });
    let mut provider = opened(&path);
    let write = om_data::ScriptWrite {
        layout: LAYOUT as u32,
        record_number: Some(RecordNumber::Window(3)),
        ..Default::default()
    };
    let preview_error = provider.preview_new_record(&write).err().unwrap();
    assert_eq!(preview_error.code, Some(error::UNAVAILABLE));
    let create_error = provider.write_record(&write).unwrap_err();
    assert_eq!(create_error.code, Some(error::UNAVAILABLE));
    let cells = [(NAME, Some("Imported".into()))].into();
    let import_error = provider
        .import_record(LAYOUT as u32, None, &cells, true)
        .unwrap_err();
    assert_eq!(import_error.code, Some(error::UNAVAILABLE));
    provider.begin_record_commit().unwrap();
    let import_error = provider
        .import_record(LAYOUT as u32, None, &cells, true)
        .unwrap_err();
    assert_eq!(import_error.code, Some(error::UNAVAILABLE));
    let create_error = provider.write_record(&write).err().unwrap();
    assert_eq!(create_error.code, Some(error::UNAVAILABLE));
    provider.revert_transaction().unwrap();
    provider.close();

    let file = om_hbam::HbamFile::open(&path).unwrap();
    let tree = file.key_tree().unwrap();
    let next = tree
        .at(&[TABLE, 3, 5, SERIAL, 0x0c])
        .unwrap()
        .text()
        .unwrap()
        .unwrap();
    assert_eq!(next, "1");
    drop(tree);
    drop(file);

    let mut provider = opened(&path);
    provider.begin_record_commit().unwrap();
    let made = provider
        .write_record(&om_data::ScriptWrite {
            record_number: Some(RecordNumber::LocalNewRecord(3)),
            ..write
        })
        .unwrap();
    provider.commit_record_session(false).unwrap();
    provider.close();

    let mut reopened = opened(&path);
    let saved = reopened.record_by_id(LAYOUT as u32, made.id).unwrap();
    assert_eq!(text(&saved, made.id, "Position"), Some("3"));
    assert_eq!(text(&saved, made.id, "Serial"), Some("1"));
}

#[test]
fn new_record_auto_enters_the_session_globals() {
    use fixture::{LAYOUT, NAME, OCCURRENCE};
    use om_data::calc::Value;
    use om_data::host::CalcNeed;
    use om_hbam::slot::uint_segment;

    const CONSTANT: u64 = 40;
    const CALCULATED: u64 = 41;
    const SERIAL: u64 = 42;
    let folder = Folder::new("global-auto-entry");
    let path = folder.file("Sample.fmp12", PASSWORD, |root| {
        for (id, label, options) in [
            (CONSTANT, "Default setting", 0x1_0004),
            (CALCULATED, "Calculated setting", 0x1_0008),
            (SERIAL, "Serial setting", 0x1_0002),
        ] {
            fixture::add_field(root, TABLE, id, label, 1, 1, 0, options, 0);
        }
        let table = uint_segment(TABLE);
        root.put(
            &[&table, &[3], &[5], &uint_segment(CONSTANT), &[0x0d]],
            om_scripts::text_node(&[], "X"),
        );
        fixture::add_formula(
            root,
            &fixture::names(),
            (TABLE, OCCURRENCE),
            CALCULATED,
            5,
            "\"Y\"",
        );
        fixture::set_field_serial_increment(root, TABLE, SERIAL, 1);
        root.put(
            &[&table, &[3], &[5], &uint_segment(SERIAL), &[0x0c]],
            om_scripts::text_node(&[], "1"),
        );
    });
    let mut provider = opened(&path);
    let globals = |provider: &mut NativeProvider| {
        provider
            .calc_data(&[CalcNeed::Globals { table: TABLE }])
            .unwrap()
            .globals[&TABLE]
            .clone()
    };
    let new = om_data::ScriptWrite {
        layout: LAYOUT as u32,
        ..Default::default()
    };
    provider.begin_record_commit().unwrap();
    let record = provider.write_record(&new).unwrap();
    let values = globals(&mut provider);
    for (field, text) in [(CONSTANT, "X"), (CALCULATED, "Y"), (SERIAL, "1")] {
        assert!(matches!(values.get(field, 1), Some(Value::Text(value)) if value.as_str() == text));
    }
    provider
        .write_record(&om_data::ScriptWrite {
            record: Some(record.id),
            cells: [(NAME, Some("A note".into()))].into(),
            ..new.clone()
        })
        .unwrap();
    provider.set_global(TABLE, CONSTANT, 1, "Changed").unwrap();
    provider.commit_transaction(false).unwrap();
    assert_eq!(
        name_of(&mut provider, LAYOUT as u32, record.id).as_deref(),
        Some("A note")
    );
    let values = globals(&mut provider);
    assert!(
        matches!(values.get(CONSTANT, 1), Some(Value::Text(value)) if value.as_str() == "Changed")
    );
    provider.begin_record_commit().unwrap();
    assert_eq!(provider.write_record(&new).unwrap_err().code, Some(3));
    provider.revert_transaction().unwrap();
    provider.close();
}

/// An auto-entered and a validation formula over a global field read the
/// session's value of it (#187, schema-path.md 5.17): Stamp is auto-entered
/// `Name & Setting` and Checked validated by `Self <> Setting` (always,
/// strict), Setting being Tasks' global text field. With Setting "x" set in
/// this session, a new record named Ada gets Stamp "Adax", and Checked "x"
/// is refused while "y" is written.
#[test]
fn option_formulas_read_the_sessions_global_value() {
    use fixture::{LAYOUT, NAME, OCCURRENCE, SETTING};
    const STAMP: u64 = 40;
    const CHECKED: u64 = 41;
    let folder = Folder::new("option-globals");
    let path = folder.file("Tasks.fmp12", PASSWORD, |root| {
        let mut names = fixture::names();
        names.add_field(TABLE, STAMP, "Stamp");
        names.add_field(TABLE, CHECKED, "Checked");
        // Auto-entered calculation `0x8`; validated by calculation, always,
        // strict `0x405`.
        fixture::add_field(root, TABLE, STAMP, "Stamp", 1, 1, 0, 0x8, 0);
        fixture::add_field(root, TABLE, CHECKED, "Checked", 1, 1, 0, 0, 0x405);
        fixture::add_formula(
            root,
            &names,
            (TABLE, OCCURRENCE),
            STAMP,
            5,
            "Name & Setting",
        );
        fixture::add_formula(
            root,
            &names,
            (TABLE, OCCURRENCE),
            CHECKED,
            0x19,
            "Self <> Setting",
        );
    });
    let mut provider = opened(&path);
    provider.set_global(TABLE, SETTING, 1, "x").unwrap();
    let record = provider
        .create_record(
            LAYOUT as u32,
            FieldChanges::from([("Name".to_owned(), Some("Ada".to_owned()))]),
        )
        .unwrap();
    let id = u64::try_from(record.id).unwrap();
    let text_of = |v: &om_data::calc::Value| match v {
        om_data::calc::Value::Text(t) => t.as_str().to_owned(),
        other => format!("{other:?}"),
    };
    let stamp = |provider: &mut NativeProvider| {
        let need = om_data::host::CalcNeed::Record {
            table: TABLE,
            record: id,
        };
        let data = provider.calc_data(&[need]).unwrap();
        let values = data.records[&(TABLE, id)].clone().unwrap();
        (
            values.get(NAME, 1).map(text_of),
            values.get(STAMP, 1).map(text_of),
            values.get(CHECKED, 1).map(text_of),
        )
    };
    assert_eq!(stamp(&mut provider).1.as_deref(), Some("Adax"));
    let check = |provider: &mut NativeProvider, text: &str| {
        provider.update_record(
            LAYOUT as u32,
            record.id,
            FieldChanges::from([("Checked".to_owned(), Some(text.to_owned()))]),
        )
    };
    assert!(
        check(&mut provider, "x").is_err(),
        "Checked equal to Setting is refused"
    );
    check(&mut provider, "y").unwrap();
    assert_eq!(stamp(&mut provider).2.as_deref(), Some("y"));
    provider.close();
}

/// A row imported inside a transaction and then edited there is computed
/// at commit with the session's global values too (#897's review): Stamp
/// auto-entered `Name & Setting`, replacing the value; Setting "x"; Ada is
/// imported and renamed Bea before the commit, which gives Stamp "Beax".
#[test]
fn an_imported_row_edited_in_a_transaction_reads_the_sessions_global_value() {
    use fixture::{LAYOUT, NAME, OCCURRENCE, SETTING};
    const STAMP: u64 = 40;
    let folder = Folder::new("import-globals");
    let path = folder.file("Tasks.fmp12", PASSWORD, |root| {
        let mut names = fixture::names();
        names.add_field(TABLE, STAMP, "Stamp");
        // Auto-entered calculation replacing the value: `0x8 | 0x80`.
        fixture::add_field(root, TABLE, STAMP, "Stamp", 1, 1, 0, 0x88, 0);
        fixture::add_formula(
            root,
            &names,
            (TABLE, OCCURRENCE),
            STAMP,
            5,
            "Name & Setting",
        );
    });
    let mut provider = opened(&path);
    provider.set_global(TABLE, SETTING, 1, "x").unwrap();
    provider
        .begin_transaction(TransactionOptions::default())
        .unwrap();
    let cells = BTreeMap::from([(NAME, Some("Ada".to_owned()))]);
    let id = provider
        .import_record(LAYOUT as u32, None, &cells, true)
        .unwrap();
    provider
        .update_record(
            LAYOUT as u32,
            id,
            FieldChanges::from([("Name".to_owned(), Some("Bea".to_owned()))]),
        )
        .unwrap();
    provider.commit_transaction(false).unwrap();
    let record = u64::try_from(id).unwrap();
    let need = om_data::host::CalcNeed::Record {
        table: TABLE,
        record,
    };
    let data = provider.calc_data(&[need]).unwrap();
    let values = data.records[&(TABLE, record)].clone().unwrap();
    match values.get(STAMP, 1) {
        Some(om_data::calc::Value::Text(t)) => assert_eq!(t.as_str(), "Beax"),
        other => panic!("Stamp: {other:?}"),
    }
    provider.close();
}

/// What Manage Database does with every field of every shipped starter
/// solution's own definition (#432, criterion 7's "changing a calculation
/// or summary field"), through the local-file path, staged and cancelled:
/// each field's dialog OK'd as it is (Specify Calculation, Options for
/// Summary Field, Options for Field), then each formula OK'd with a comment
/// `/* edited */` appended, which changes no result; saved, reopened, and
/// each edited formula read back: 2,695 changes on the six starters in 11
/// languages, none refused. Prints any refusals by reason, ranked.
#[test]
#[ignore = "requires the FileMaker Pro 26 starter solutions in re/extracted or OM_TEMPLATES"]
fn every_starter_field_definition_is_changed_or_refused() {
    use om_data::field_options::{AutoEnterValue, FieldOptions};
    use om_data::SchemaChange;
    let folder = Folder::new("change-survey");
    let mut tried = 0usize;
    let mut refused: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    let edit = |text: &str| format!("{text} /* edited */");
    for template in starter_files() {
        let at = template.display().to_string();
        let file = at
            .rsplit("DynamicTemplates/")
            .next()
            .unwrap_or(&at)
            .to_owned();
        let path = folder.0.join("starter.fmp12");
        std::fs::copy(&template, &path).unwrap();
        let mut provider = clean_client();
        provider
            .open_local(&local(&path, Some(("Admin", ""))))
            .unwrap_or_else(|e| panic!("{at}: {}", e.message));
        let schema = provider.begin_schema_edit().unwrap();
        let mut edited: Vec<(u64, u64, &'static str, String)> = Vec::new();
        for table in &schema.tables {
            for field in &table.fields {
                // Each change, and the formulas it edits, to read back.
                let mut changes: Vec<(&str, SchemaChange, Vec<&'static str>)> = Vec::new();
                if let Some(calculation) = &field.calculation {
                    changes.push((
                        "calculation as it is",
                        SchemaChange::ChangeCalculation {
                            table_id: table.id,
                            field_id: field.id,
                            calculation: calculation.clone(),
                        },
                        Vec::new(),
                    ));
                    let mut edited = calculation.clone();
                    edited.formula = edit(&calculation.formula);
                    changes.push((
                        "calculation edited",
                        SchemaChange::ChangeCalculation {
                            table_id: table.id,
                            field_id: field.id,
                            calculation: edited,
                        },
                        vec!["calculation"],
                    ));
                } else if let Some(summary) = &field.summary {
                    changes.push((
                        "summary as it is",
                        SchemaChange::ChangeSummary {
                            table_id: table.id,
                            field_id: field.id,
                            summary: *summary,
                        },
                        Vec::new(),
                    ));
                } else if let Some(options) = &field.options {
                    changes.push((
                        "options as they are",
                        SchemaChange::SetFieldOptions {
                            table_id: table.id,
                            field_id: field.id,
                            options: Box::new(options.clone()),
                        },
                        Vec::new(),
                    ));
                    let mut edited: FieldOptions = options.clone();
                    let mut kinds: Vec<&'static str> = Vec::new();
                    if let AutoEnterValue::Calculated(c) = &mut edited.auto_enter.value {
                        if let Some(text) = &c.formula.text {
                            c.formula.text = Some(edit(text));
                            kinds.push("auto-enter");
                        }
                    }
                    if let Some(c) = &mut edited.validation.calculation {
                        if let Some(text) = &c.formula.text {
                            c.formula.text = Some(edit(text));
                            kinds.push("validation");
                        }
                    }
                    if !kinds.is_empty() {
                        changes.push((
                            "option formula edited",
                            SchemaChange::SetFieldOptions {
                                table_id: table.id,
                                field_id: field.id,
                                options: Box::new(edited),
                            },
                            kinds,
                        ));
                    }
                }
                for (what, change, kinds) in changes {
                    tried += 1;
                    let applied = provider.apply_schema_change(change);
                    if applied.is_ok() {
                        for kind in kinds {
                            edited.push((
                                table.id,
                                field.id,
                                kind,
                                format!("{}::{}", table.name, field.name),
                            ));
                        }
                    }
                    if let Err(e) = applied {
                        let reason = format!(
                            "{what}: {}",
                            e.message
                                .replace(&field.name, "<field>")
                                .replace(&table.name, "<table>")
                        );
                        refused
                            .entry(reason)
                            .or_default()
                            .push(format!("{file}: {}::{}", table.name, field.name));
                    }
                }
            }
        }
        // Saved, then read back: each formula edited holds the comment.
        if let Err(e) = provider.commit_schema_changes() {
            refused
                .entry(format!("save: {}", e.message))
                .or_default()
                .push(file.clone());
            provider.close();
            continue;
        }
        provider.close();
        let mut provider = clean_client();
        provider
            .open_local(&local(&path, Some(("Admin", ""))))
            .unwrap_or_else(|e| panic!("{at} reopened: {}", e.message));
        // Every formula edited is there, with its edit.
        let schema = provider.database_schema().unwrap();
        for (table_id, field_id, kind, which) in edited {
            let field = schema
                .tables
                .iter()
                .find(|t| t.id == table_id)
                .and_then(|t| t.fields.iter().find(|f| f.id == field_id));
            let text = field.and_then(|f| match kind {
                "calculation" => f.calculation.as_ref().map(|c| c.formula.clone()),
                "auto-enter" => match f.options.as_ref().map(|o| &o.auto_enter.value) {
                    Some(AutoEnterValue::Calculated(c)) => c.formula.text.clone(),
                    _ => None,
                },
                _ => f
                    .options
                    .as_ref()
                    .and_then(|o| o.validation.calculation.as_ref())
                    .and_then(|c| c.formula.text.clone()),
            });
            if !text.is_some_and(|t| t.contains("/* edited */")) {
                refused
                    .entry(format!("read back after reopening: the {kind} formula, missing or without its edit"))
                    .or_default()
                    .push(format!("{file}: {which}"));
            }
        }
        provider.close();
    }
    let total: usize = refused.values().map(Vec::len).sum();
    println!("{tried} changes tried, {total} refused");
    let mut ranked: Vec<_> = refused.iter().collect();
    ranked.sort_by_key(|a| std::cmp::Reverse(a.1.len()));
    for (reason, fields) in ranked {
        let english: Vec<&String> = fields.iter().filter(|f| f.contains("/en.fmp12")).collect();
        println!(
            "{:5} ({} English) {reason}\n      e.g. {}",
            fields.len(),
            english.len(),
            english.first().copied().unwrap_or(&fields[0])
        );
        for f in english.iter().skip(1).take(8) {
            println!("           {f}");
        }
    }
    assert!(tried > 0, "no starter solution was found");
    assert_eq!(total, 0, "{refused:#?}");
}

/// Renames and relationship edits on every shipped starter solution
/// (#432, criterion 7), through the local-file path: every field renamed
/// to its name with " X" appended (Change in the Fields tab), every
/// relationship OK'd as it is (Edit Relationship), saved, reopened, and
/// read back: the new names, the relationships unchanged, and every
/// calculation's formula still readable. Every relationship is kept
/// (Assets' sorted relationship 1, which a lookup follows, included);
/// renames are refused only where the new name would need `${…}`. Prints
/// the refusals by reason, ranked.
#[test]
#[ignore = "requires the FileMaker Pro 26 starter solutions in re/extracted or OM_TEMPLATES"]
fn every_starter_field_is_renamed_and_relationship_kept_or_refused() {
    use om_data::{RelationshipMatch, SchemaChange};
    type Refused = std::collections::BTreeMap<String, Vec<String>>;
    fn note(refused: &mut Refused, what: String, which: String) {
        refused.entry(what).or_default().push(which);
    }
    let folder = Folder::new("rename-survey");
    let mut tried = 0usize;
    let mut refused = Refused::new();
    for template in starter_files() {
        let at = template.display().to_string();
        let file = at
            .rsplit("DynamicTemplates/")
            .next()
            .unwrap_or(&at)
            .to_owned();
        let path = folder.0.join("starter.fmp12");
        std::fs::copy(&template, &path).unwrap();
        let mut provider = clean_client();
        provider
            .open_local(&local(&path, Some(("Admin", ""))))
            .unwrap_or_else(|e| panic!("{at}: {}", e.message));
        let before = provider.begin_schema_edit().unwrap();
        let mut not_renamed = std::collections::BTreeSet::new();
        for table in &before.tables {
            for field in &table.fields {
                tried += 1;
                let renamed = provider.apply_schema_change(SchemaChange::ChangeField {
                    table_id: table.id,
                    field_id: field.id,
                    name: format!("{} X", field.name),
                    data_type: field.data_type,
                    comment: field.comment.clone(),
                });
                if let Err(e) = renamed {
                    not_renamed.insert((table.id, field.id));
                    note(
                        &mut refused,
                        format!("rename: {}", e.message.replace(&field.name, "<field>")),
                        format!("{file}: {}::{}", table.name, field.name),
                    );
                }
            }
        }
        for relationship in &before.relationships {
            tried += 1;
            let matches = relationship
                .matches
                .iter()
                .map(|m| RelationshipMatch {
                    left_field: m.left_field,
                    operator: m.operator,
                    right_field: m.right_field,
                })
                .collect();
            let kept = provider.apply_schema_change(SchemaChange::ChangeRelationship {
                relationship_id: relationship.id,
                matches,
                left: relationship.left(),
                right: relationship.right(),
            });
            if let Err(e) = kept {
                note(
                    &mut refused,
                    format!("relationship as it is: {}", e.message),
                    format!("{file}: relationship {}", relationship.id),
                );
            }
        }
        if let Err(e) = provider.commit_schema_changes() {
            note(&mut refused, format!("save: {}", e.message), file.clone());
            provider.close();
            continue;
        }
        provider.close();
        let mut provider = clean_client();
        provider
            .open_local(&local(&path, Some(("Admin", ""))))
            .unwrap_or_else(|e| panic!("{at} reopened: {}", e.message));
        let after = provider.database_schema().unwrap();
        for table in &before.tables {
            let Some(now) = after.tables.iter().find(|t| t.id == table.id) else {
                note(
                    &mut refused,
                    "read back: a table is missing".into(),
                    file.clone(),
                );
                continue;
            };
            for field in &table.fields {
                let which = format!("{file}: {}::{}", table.name, field.name);
                let Some(f) = now.fields.iter().find(|f| f.id == field.id) else {
                    note(&mut refused, "read back: a field is missing".into(), which);
                    continue;
                };
                if f.name != format!("{} X", field.name)
                    && !not_renamed.contains(&(table.id, field.id))
                {
                    note(
                        &mut refused,
                        "read back: the name is not the new one".into(),
                        which.clone(),
                    );
                }
                if field.calculation.is_some() && f.calculation.is_none() {
                    note(
                        &mut refused,
                        "read back: the calculation's formula no longer reads".into(),
                        which,
                    );
                }
            }
        }
        let pairs = |r: &om_data::SchemaRelationship| -> Vec<(u64, u64, u8)> {
            r.matches
                .iter()
                .map(|m| (m.left_field, m.right_field, m.operator))
                .collect()
        };
        for relationship in &before.relationships {
            let same = after.relationships.iter().any(|r| {
                r.id == relationship.id
                    && r.options == relationship.options
                    && r.orders == relationship.orders
                    && pairs(r) == pairs(relationship)
            });
            if !same {
                note(
                    &mut refused,
                    "read back: the relationship changed".into(),
                    format!("{file}: relationship {}", relationship.id),
                );
            }
        }
        provider.close();
    }
    let total: usize = refused.values().map(Vec::len).sum();
    println!("{tried} changes tried, {total} refused or not read back");
    let mut ranked: Vec<_> = refused.iter().collect();
    ranked.sort_by_key(|a| std::cmp::Reverse(a.1.len()));
    for (reason, fields) in ranked {
        let english: Vec<&String> = fields.iter().filter(|f| f.contains("/en.fmp12")).collect();
        println!(
            "{:5} ({} English) {reason}\n      e.g. {}",
            fields.len(),
            english.len(),
            english.first().copied().unwrap_or(&fields[0])
        );
        for f in english.iter().skip(1).take(8) {
            println!("           {f}");
        }
    }
    assert!(tried > 0, "no starter solution was found");
    // Only renames to a name calculations would quote are refused (39
    // fields of the other languages' starters, whose names already hold a
    // delimiter); what FileMaker Pro does with such a name is not measured.
    let unexpected: Vec<_> = refused
        .keys()
        .filter(|reason| !reason.starts_with("rename: ") || !reason.contains("${…}"))
        .collect();
    assert!(unexpected.is_empty(), "{unexpected:#?}");
}

/// The everyday Manage Database edits of the English starter solutions'
/// own schemas, each staged on its own and cancelled (#432, criterion 7):
/// rename every table and every occurrence; delete every field,
/// relationship, occurrence and table; on every normal field, change its
/// indexing, give it two repetitions, and change its type to Number (Text
/// for a number field). Prints the refusals by edit and reason, ranked;
/// asserts only that starters were found.
#[test]
#[ignore = "requires the FileMaker Pro 26 starter solutions in re/extracted or OM_TEMPLATES"]
fn every_english_starter_schema_edit_is_made_or_refused() {
    use om_data::field_options::Indexing;
    use om_data::{FieldKind, SchemaChange, SchemaFieldKind};
    type Refused = std::collections::BTreeMap<String, Vec<String>>;
    let folder = Folder::new("schema-edit-survey");
    let mut tried = 0usize;
    let mut refused = Refused::new();
    for template in starter_files()
        .into_iter()
        .filter(|t| t.file_name().is_some_and(|n| n == "en.fmp12"))
    {
        let at = template.display().to_string();
        let file = at
            .rsplit("DynamicTemplates/")
            .next()
            .unwrap_or(&at)
            .to_owned();
        let path = folder.0.join("starter.fmp12");
        std::fs::copy(&template, &path).unwrap();
        let mut provider = clean_client();
        provider
            .open_local(&local(&path, Some(("Admin", ""))))
            .unwrap_or_else(|e| panic!("{at}: {}", e.message));
        let schema = provider.begin_schema_edit().unwrap();
        // How many records each table holds: a field of an empty table holds
        // no value to convert.
        for table in &schema.tables {
            let need = om_data::host::CalcNeed::Records { table: table.id };
            let records = provider
                .calc_data(&[need])
                .ok()
                .and_then(|d| d.tables.get(&table.id).map(|r| r.len()));
            println!("{file}: {} holds {records:?} records", table.name);
        }
        let mut edits: Vec<(String, String, SchemaChange)> = Vec::new();
        for table in &schema.tables {
            edits.push((
                "rename table".into(),
                table.name.clone(),
                SchemaChange::ChangeTable {
                    table_id: table.id,
                    name: format!("{} X", table.name),
                    comment: table.comment.clone(),
                },
            ));
            edits.push((
                "delete table".into(),
                table.name.clone(),
                SchemaChange::DeleteTable {
                    table_id: table.id,
                    remove_occurrences: true,
                },
            ));
            for field in &table.fields {
                let which = format!("{}::{}", table.name, field.name);
                edits.push((
                    "delete field".into(),
                    which.clone(),
                    SchemaChange::DeleteField {
                        table_id: table.id,
                        field_id: field.id,
                    },
                ));
                let (SchemaFieldKind::Normal, Some(options)) = (field.kind, &field.options) else {
                    continue;
                };
                let mut indexed = options.clone();
                indexed.storage.indexing = match options.storage.indexing {
                    Indexing::None => Indexing::All,
                    _ => Indexing::None,
                };
                // FileMaker indexes no container: not an edit to try.
                if field.data_type != FieldKind::Container {
                    edits.push((
                        "change indexing".into(),
                        which.clone(),
                        SchemaChange::SetFieldOptions {
                            table_id: table.id,
                            field_id: field.id,
                            options: Box::new(indexed),
                        },
                    ));
                }
                let mut repeating = options.clone();
                repeating.storage.repetitions = 2;
                edits.push((
                    "two repetitions".into(),
                    which.clone(),
                    SchemaChange::SetFieldOptions {
                        table_id: table.id,
                        field_id: field.id,
                        options: Box::new(repeating),
                    },
                ));
                let other = if field.data_type == FieldKind::Number {
                    FieldKind::Text
                } else {
                    FieldKind::Number
                };
                edits.push((
                    "change type".into(),
                    which,
                    SchemaChange::ChangeField {
                        table_id: table.id,
                        field_id: field.id,
                        name: field.name.clone(),
                        data_type: other,
                        comment: field.comment.clone(),
                    },
                ));
            }
        }
        for occurrence in &schema.occurrences {
            edits.push((
                "rename occurrence".into(),
                occurrence.name.clone(),
                SchemaChange::RenameOccurrence {
                    occurrence_id: occurrence.id,
                    name: format!("{} X", occurrence.name),
                },
            ));
            edits.push((
                "delete occurrence".into(),
                occurrence.name.clone(),
                SchemaChange::DeleteOccurrence {
                    occurrence_id: occurrence.id,
                },
            ));
        }
        for relationship in &schema.relationships {
            edits.push((
                "delete relationship".into(),
                format!("relationship {}", relationship.id),
                SchemaChange::DeleteRelationship {
                    relationship_id: relationship.id,
                },
            ));
        }
        provider.cancel_schema_changes().unwrap();
        for (what, which, change) in edits {
            tried += 1;
            provider.begin_schema_edit().unwrap();
            if let Err(e) = provider.apply_schema_change(change) {
                let name = which.rsplit("::").next().unwrap_or(&which).to_owned();
                refused
                    .entry(format!("{what}: {}", e.message.replace(&name, "<name>")))
                    .or_default()
                    .push(format!("{file}: {which}"));
            }
            provider.cancel_schema_changes().unwrap();
        }
        provider.close();
    }
    let total: usize = refused.values().map(Vec::len).sum();
    println!("{tried} edits tried, {total} refused");
    let mut ranked: Vec<_> = refused.iter().collect();
    ranked.sort_by_key(|a| std::cmp::Reverse(a.1.len()));
    for (reason, cases) in ranked {
        println!("{:5} {reason}\n      e.g. {}", cases.len(), cases[0]);
        for c in cases.iter().skip(1).take(5) {
            println!("           {c}");
        }
    }
    assert!(tried > 0, "no starter solution was found");
}

/// A field of a table holding no record changes its type to Container on a
/// file on this computer, though what its stored text would read as there
/// is not established (#1268): there is none. The save checks under the
/// table's lock that none was added meanwhile, by another connection or in
/// this provider's own transaction, and otherwise sends nothing (#432). A
/// table with records its record list does not name keeps the refusal, and
/// a hosted file's empty table keeps it for a change to Date, which the
/// host may not convert.
#[test]
fn an_empty_tables_field_changes_type_unless_a_record_appears_before_the_save() {
    use fixture::{BODY, NOTES, NOTES_LAYOUT};
    use om_data::{FieldKind, SchemaChange};
    let folder = Folder::new("empty-table-type");
    // Notes, emptied: no record and an empty record list.
    let emptied = || {
        folder.file("Tasks.fmp12", PASSWORD, |root| {
            let notes = om_hbam::slot::uint_segment(NOTES);
            root.remove(&[&notes, &[5]]);
            root.remove(&[&notes, &[0x0d]]);
        })
    };
    let path = emptied();
    let to_date = SchemaChange::ChangeField {
        table_id: NOTES,
        field_id: BODY,
        name: "Body".into(),
        data_type: FieldKind::Date,
        comment: String::new(),
    };
    let to_container = SchemaChange::ChangeField {
        table_id: NOTES,
        field_id: BODY,
        name: "Body".into(),
        data_type: FieldKind::Container,
        comment: String::new(),
    };
    let body_type = |provider: &mut NativeProvider| {
        provider
            .database_schema()
            .unwrap()
            .tables
            .iter()
            .find(|t| t.id == NOTES)
            .and_then(|t| t.fields.iter().find(|f| f.id == BODY))
            .map(|f| f.data_type)
            .unwrap()
    };
    let note = |text: &str| FieldChanges::from([("Body".to_owned(), Some(text.to_owned()))]);

    // Another connection adds a record after the edit began: refused at
    // the save, nothing sent.
    let mut provider = opened(&path);
    let mut other = provider.open_peer("Tasks").unwrap();
    provider.begin_schema_edit().unwrap();
    provider.apply_schema_change(to_container.clone()).unwrap();
    other
        .create_record(NOTES_LAYOUT as u32, note("hi"))
        .unwrap();
    let refused = provider.commit_schema_changes().unwrap_err();
    assert!(
        refused.message.contains("A record was added"),
        "{}",
        refused.message
    );
    provider.cancel_schema_changes().unwrap();
    assert_eq!(body_type(&mut provider), FieldKind::Text);
    other.close();
    provider.close();

    // Another connection holds a new record it has not committed: the
    // table's lock is refused, nothing sent.
    let path = emptied();
    let mut provider = opened(&path);
    let mut other = provider.open_peer("Tasks").unwrap();
    provider.begin_schema_edit().unwrap();
    provider.apply_schema_change(to_container.clone()).unwrap();
    other
        .begin_transaction(TransactionOptions::default())
        .unwrap();
    other
        .create_record(NOTES_LAYOUT as u32, note("held"))
        .unwrap();
    let refused = provider.commit_schema_changes().unwrap_err();
    assert!(
        refused.message.contains("is in use by"),
        "{}",
        refused.message
    );
    provider.cancel_schema_changes().unwrap();
    assert_eq!(body_type(&mut provider), FieldKind::Text);
    other.close();

    // This provider holds a new record in its own transaction: refused.
    provider.begin_schema_edit().unwrap();
    provider.apply_schema_change(to_container.clone()).unwrap();
    provider
        .begin_transaction(TransactionOptions::default())
        .unwrap();
    provider
        .create_record(NOTES_LAYOUT as u32, note("own"))
        .unwrap();
    let refused = provider.commit_schema_changes().unwrap_err();
    assert!(
        refused.message.contains("A record was added"),
        "{}",
        refused.message
    );
    provider.cancel_schema_changes().unwrap();
    provider.close();

    // Nothing added: saved, and read back after reopening.
    let path = emptied();
    let mut provider = opened(&path);
    provider.begin_schema_edit().unwrap();
    provider.apply_schema_change(to_container.clone()).unwrap();
    provider.commit_schema_changes().unwrap();
    provider.close();
    let mut provider = opened(&path);
    assert_eq!(body_type(&mut provider), FieldKind::Container);
    provider.close();

    // Records kept under `[T, 05]` though the record list `[T, 0d]` is
    // gone, as a damaged file might: not empty, refused.
    let damaged = folder.file("Tasks.fmp12", PASSWORD, |root| {
        root.remove(&[&om_hbam::slot::uint_segment(NOTES), &[0x0d]]);
    });
    let mut provider = opened(&damaged);
    provider.begin_schema_edit().unwrap();
    let refused = provider
        .apply_schema_change(to_container.clone())
        .unwrap_err();
    assert!(refused.message.contains("#1268"), "{}", refused.message);
    provider.cancel_schema_changes().unwrap();
    provider.close();

    // Hosted, the same empty table keeps the refusal of a change to Date:
    // another client could add a record a host that does not convert would
    // leave as it is.
    let path = emptied();
    let (port, ca_pem) = om_server(&path, &folder.0.join("recordings"));
    let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
    let mut hosted = NativeProvider::new(NativeConfig {
        client_cert_pem: client.cert.pem().into_bytes(),
        client_key_pem: client.key_pair.serialize_pem().into_bytes(),
        additional_ca_pem: ca_pem,
        callback_host: "127.0.0.1".into(),
        machine: MachineInfo::default(),
    });
    hosted.set_client_license(None).unwrap();
    hosted
        .open(&Connection {
            host: "127.0.0.1".into(),
            port,
            file: FILE.into(),
            account: ACCOUNT.into(),
            password: PASSWORD.into(),
        })
        .unwrap();
    hosted.begin_schema_edit().unwrap();
    let refused = hosted.apply_schema_change(to_date).unwrap_err();
    assert!(refused.message.contains("#1267"), "{}", refused.message);
    hosted.cancel_schema_changes().unwrap();
    hosted.close();
}

/// A field holding values takes another index language on a file on this
/// computer, whose host keys its index again in it (`0x10`, schema-path.md
/// 7.2; #431): Tasks::Name, indexed in English, made Unicode raw in a
/// later edit, is keyed as its code units (query-program.md 7.2). A
/// language whose keys OpenMaker does not build (German, 25) is refused
/// first, changing nothing (#159).
#[test]
fn a_fields_index_language_changes_and_its_index_is_keyed_in_it() {
    use om_data::field_options::Indexing;
    use om_data::SchemaChange;
    use om_query::keys::{record_value_entries, TextKeys};
    let folder = Folder::new("index-language");
    let path = folder.file(FILE, PASSWORD, index_schema_catalogs);
    let values = ["Ship", "Äpfel"];
    let mut provider = opened(&path);
    let tasks = layout(&mut provider, "Tasks");
    for id in [1, 2] {
        provider.delete_record(tasks, id).unwrap();
    }
    for value in values {
        provider.create_record(tasks, name(value)).unwrap();
    }
    let options_of = |provider: &mut NativeProvider| {
        provider
            .database_schema()
            .unwrap()
            .table(TABLE)
            .and_then(|t| t.fields.iter().find(|f| f.id == fixture::NAME))
            .and_then(|f| f.options.clone())
            .unwrap()
    };
    let set = |options| SchemaChange::SetFieldOptions {
        table_id: TABLE,
        field_id: fixture::NAME,
        options: Box::new(options),
    };
    let keys = |form| -> std::collections::BTreeSet<Vec<u8>> {
        values
            .iter()
            .flat_map(|value| {
                record_value_entries(
                    om_hbam::DataType::Text,
                    &[(*value).to_owned()],
                    om_collate::DateOrder::MonthDayYear,
                    form,
                )
            })
            .collect()
    };
    let on_disk = |path: &Path| -> std::collections::BTreeSet<Vec<u8>> {
        name_on_disk(path).keys.into_iter().collect()
    };
    provider.begin_schema_edit().unwrap();
    let mut options = options_of(&mut provider);
    assert_eq!(options.storage.language, 21);
    options.storage.indexing = Indexing::Minimal;
    provider.apply_schema_change(set(options)).unwrap();
    provider.commit_schema_changes().unwrap();
    provider.close();
    assert_eq!(on_disk(&path), keys(TextKeys::Collated));

    let mut provider = opened(&path);
    provider.begin_schema_edit().unwrap();
    let mut options = options_of(&mut provider);
    let mut german = options.clone();
    german.storage.language = 25;
    let refused = provider.apply_schema_change(set(german)).unwrap_err();
    assert_eq!(refused.code, Some(3), "{refused:?}");
    assert!(refused.message.contains("#1279"), "{refused:?}");
    options.storage.language = 2;
    provider.apply_schema_change(set(options)).unwrap();
    provider.commit_schema_changes().unwrap();
    provider.close();
    assert_ne!(keys(TextKeys::Raw), keys(TextKeys::Collated));
    assert_eq!(on_disk(&path), keys(TextKeys::Raw));
    let mut provider = opened(&path);
    assert_eq!(options_of(&mut provider).storage.language, 2);
    provider.close();
}

/// A field that holds values changes its type and repetitions on a file on
/// this computer, whose host is OpenMaker's own and converts or deletes
/// them (#441, #491; schema-path.md 7.2, 7.2.1). Notes::Body, a text
/// holding `1/2/03`, is made a timestamp and then a date in one edit: it
/// ends a date holding `1/2/2003` (as a timestamp the date alone would have
/// stayed `1/2/03`). Tasks::Name, a text holding `1/2/03` too, is made a
/// date and back to text: nothing of the undone change reaches the host,
/// and it keeps `1/2/03`. Tasks::Due, a date of two repetitions holding
/// `10/1/2026` and `10/2/2026`, becomes text of one: it keeps `10/1/2026`,
/// as every change from a date keeps the stored text, and its second
/// repetition is deleted from the file.
#[test]
fn a_fields_type_and_repetitions_change_on_a_file_on_this_computer() {
    use fixture::{BODY, DUE, NAME, NOTES, NOTES_LAYOUT, TABLE};
    use om_data::{FieldKind, SchemaChange};
    use om_hbam::slot::uint_segment;
    let folder = Folder::new("populated-type");
    // Repetition n of field F is the record cell F followed by the byte n
    // (calc-values.md, "Repetitions in records").
    let second_due = [uint_segment(DUE), vec![2]].concat();
    let path = folder.file("Tasks.fmp12", PASSWORD, |root| {
        let record = |table| [uint_segment(table), vec![5], om_hbam::serial::encode(1)];
        for (table, field) in [(NOTES, BODY), (TABLE, NAME)] {
            let [t, r, n] = record(table);
            root.put(
                &[&t, &r, &n, &uint_segment(field)],
                om_scripts::text_node(&[], "1/2/03"),
            );
        }
        let [t, r, n] = record(TABLE);
        root.put(
            &[&t, &r, &n, &second_due],
            om_scripts::text_node(&[], "10/2/2026"),
        );
        let definition: [&[u8]; 5] = [&t, &[3], &[5], &uint_segment(DUE), &[2]];
        let mut bytes = root.at(&definition).unwrap().value.clone().unwrap();
        bytes[24..26].copy_from_slice(&2u16.to_be_bytes());
        root.put(&definition, om_hbam::OwnedNode::leaf(Vec::new(), bytes));
    });
    let second_due_on_disk = || {
        let file = om_hbam::HbamFile::open(&path).unwrap();
        let tree = file.key_tree().unwrap();
        let record = om_hbam::serial::encode(1);
        let path: [&[u8]; 4] = [&uint_segment(TABLE), &[5], &record, &second_due];
        tree.node(&path).is_some()
    };
    assert!(second_due_on_disk());
    let change = |table_id, field_id, name: &str, data_type| SchemaChange::ChangeField {
        table_id,
        field_id,
        name: name.into(),
        data_type,
        comment: String::new(),
    };
    let mut provider = opened(&path);
    provider.begin_schema_edit().unwrap();
    for staged in [
        change(NOTES, BODY, "Body", FieldKind::Timestamp),
        change(NOTES, BODY, "Body", FieldKind::Date),
        change(TABLE, NAME, "Name", FieldKind::Date),
        change(TABLE, NAME, "Name", FieldKind::Text),
        change(TABLE, DUE, "Due", FieldKind::Text),
    ] {
        provider.apply_schema_change(staged).unwrap();
    }
    let mut options = provider
        .database_schema()
        .unwrap()
        .table(TABLE)
        .and_then(|t| t.fields.iter().find(|f| f.id == DUE))
        .and_then(|f| f.options.clone())
        .unwrap();
    assert_eq!(options.storage.repetitions, 2);
    options.storage.repetitions = 1;
    provider
        .apply_schema_change(SchemaChange::SetFieldOptions {
            table_id: TABLE,
            field_id: DUE,
            options: Box::new(options),
        })
        .unwrap();
    provider.commit_schema_changes().unwrap();
    provider.close();

    let mut provider = opened(&path);
    let value = |provider: &mut NativeProvider, layout: u32, name: &str| {
        let page = provider.records(layout, 0, 10).unwrap();
        let at = page.fields.iter().position(|f| f.name == name).unwrap();
        page.records[0].values[at].clone()
    };
    assert_eq!(
        value(&mut provider, NOTES_LAYOUT as u32, "Body").as_deref(),
        Some("1/2/2003")
    );
    assert_eq!(
        value(&mut provider, fixture::LAYOUT as u32, "Name").as_deref(),
        Some("1/2/03")
    );
    assert_eq!(
        value(&mut provider, fixture::LAYOUT as u32, "Due").as_deref(),
        Some("10/1/2026")
    );
    let schema = provider.database_schema().unwrap();
    let kind = |table: u64, field: u64| {
        schema
            .tables
            .iter()
            .find(|t| t.id == table)
            .and_then(|t| t.fields.iter().find(|f| f.id == field))
            .map(|f| f.data_type)
            .unwrap()
    };
    assert_eq!(
        [kind(NOTES, BODY), kind(TABLE, NAME), kind(TABLE, DUE)],
        [FieldKind::Date, FieldKind::Text, FieldKind::Text]
    );
    provider.close();
    assert!(!second_due_on_disk(), "the second repetition is deleted");
}

/// Related files beside a file on this computer share the budget of 128
/// Opens with a host's (#748's re-check): 129 existing siblings open 128,
/// and the window is told once that the rest were not opened.
#[test]
fn at_most_128_local_related_files_are_opened() {
    let folder = Folder::new("local-limit");
    for n in 0..129u64 {
        folder.file(&format!("Sibling {n}.fmp12"), PASSWORD, |_| {});
    }
    let root_path = folder.file("Root.fmp12", PASSWORD, |root| {
        for n in 0..129u64 {
            related::reference(
                root,
                (0xd1_0000 + n, &format!("Sibling {n} rows")),
                n + 1,
                &format!("Sibling {n}"),
            );
        }
    });
    let mut root = opened(&root_path);
    let schema = root.calc_schema().unwrap().unwrap();
    assert_eq!(
        schema.tables().count(),
        2 * 129,
        "Root and 128 related files, two tables each"
    );
    let warnings = root.related_file_warnings();
    assert_eq!(
        warnings
            .iter()
            .filter(|w| w.contains("more than 128 related files"))
            .count(),
        1,
        "{warnings:?}"
    );
    root.close();
}

/// Fields with options of their own deleted through the local-file path
/// (#558), for FileMaker's own tools to check afterwards (schema-path.md
/// 5.15): in a copy of the Contacts starter solution, a record is written
/// so the stored calculation Initial holds a value; Initial and CreatedBy
/// (an entry option, validated not empty) are deleted and a calculation
/// and a summary field made and saved; a second session deletes those two.
#[test]
#[ignore = "requires OM_DELETE_FIELDS_FILE, a writable copy of the Contacts starter solution (Admin, no password)"]
fn fields_with_options_are_deleted_in_a_copy_of_a_filemaker_file() {
    use om_data::calc_field::CalculationField;
    use om_data::meta::{SummaryDef, SummaryKind};
    use om_data::{DatabaseSchema, SchemaChange};
    let path = PathBuf::from(std::env::var_os("OM_DELETE_FIELDS_FILE").expect("a file"));
    let open = || {
        let mut provider = clean_client();
        provider
            .open_local(&local(&path, Some(("Admin", ""))))
            .unwrap();
        provider
    };
    let table = |schema: &DatabaseSchema| {
        schema
            .tables
            .iter()
            .find(|t| t.name == "Contacts")
            .unwrap()
            .clone()
    };
    let field = |schema: &DatabaseSchema, name: &str| {
        table(schema)
            .fields
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("no field {name}"))
            .id
    };
    let delete = |provider: &mut NativeProvider, names: &[&str]| {
        let mut schema = provider.begin_schema_edit().unwrap();
        for name in names {
            schema = provider
                .apply_schema_change(SchemaChange::DeleteField {
                    table_id: table(&schema).id,
                    field_id: field(&schema, name),
                })
                .unwrap_or_else(|e| panic!("{name}: {}", e.message));
        }
        schema
    };

    let mut provider = open();
    let details = layout(&mut provider, "Contact Details");
    provider
        .create_record(
            details,
            FieldChanges::from([
                ("First Name".to_owned(), Some("Ada".to_owned())),
                ("Last Name".to_owned(), Some("Lovelace".to_owned())),
            ]),
        )
        .unwrap();
    let schema = delete(&mut provider, &["Initial", "CreatedBy"]);
    let contacts = table(&schema).id;
    let context = schema
        .occurrences
        .iter()
        .find(|o| o.name == "Contacts")
        .unwrap()
        .id;
    let schema = provider
        .apply_schema_change(SchemaChange::CreateCalculationField {
            table_id: contacts,
            name: "Full Name".into(),
            comment: String::new(),
            calculation: CalculationField::new("First Name & \" \" & Last Name", context),
        })
        .unwrap();
    provider
        .apply_schema_change(SchemaChange::CreateSummaryField {
            table_id: contacts,
            name: "Names".into(),
            comment: String::new(),
            summary: SummaryDef::new(SummaryKind::Count, field(&schema, "First Name")),
        })
        .unwrap();
    provider.commit_schema_changes().unwrap();
    provider.close();

    let mut provider = open();
    delete(&mut provider, &["Full Name", "Names"]);
    provider.commit_schema_changes().unwrap();
    provider.close();
}

/// Calculation fields over a global and a summary field of their own table
/// made through the local-file path (#187), for FileMaker's own tools to
/// check afterwards (schema-path.md 5.16): in a copy of the Contacts
/// starter solution, a record Ada Lovelace; the global text field G, the
/// number field N, the summaries SumN (Total of N) and Names (Count of
/// First Name), and the calculations Greeting (`First Name & G`), Tagged
/// (`G & "x"`), Share (`N / SumN`, a number) and Counted (`Names`, a
/// number); saved. A second session reads them back: each calculation is
/// unstored, as FileMaker's engine makes it.
#[test]
#[ignore = "requires OM_CALC_GLOBAL_SUMMARY_FILE, a writable copy of the Contacts starter solution (Admin, no password)"]
fn calculations_over_globals_and_summaries_are_made_in_a_copy_of_a_filemaker_file() {
    use om_data::calc_field::CalculationField;
    use om_data::field_options::Indexing;
    use om_data::meta::{SummaryDef, SummaryKind};
    use om_data::{DatabaseSchema, FieldColumn, FieldKind, SchemaChange, SchemaField, SchemaTable};
    let path = PathBuf::from(std::env::var_os("OM_CALC_GLOBAL_SUMMARY_FILE").expect("a file"));
    let open = || {
        let mut provider = clean_client();
        provider
            .open_local(&local(&path, Some(("Admin", ""))))
            .unwrap();
        provider
    };
    let contacts = |schema: &DatabaseSchema| -> SchemaTable {
        schema
            .tables
            .iter()
            .find(|t| t.name == "Contacts")
            .unwrap()
            .clone()
    };
    let field = |schema: &DatabaseSchema, name: &str| -> SchemaField {
        contacts(schema)
            .fields
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("no field {name}"))
            .clone()
    };
    let apply = |provider: &mut NativeProvider, change: SchemaChange| {
        provider
            .apply_schema_change(change)
            .unwrap_or_else(|e| panic!("{}", e.message))
    };

    let mut provider = open();
    let details = layout(&mut provider, "Contact Details");
    provider
        .create_record(
            details,
            FieldChanges::from([
                ("First Name".to_owned(), Some("Ada".to_owned())),
                ("Last Name".to_owned(), Some("Lovelace".to_owned())),
            ]),
        )
        .unwrap();
    let mut schema = provider.begin_schema_edit().unwrap();
    let table = contacts(&schema).id;
    let context = schema
        .occurrences
        .iter()
        .find(|o| o.name == "Contacts")
        .unwrap()
        .id;
    for (name, data_type) in [("G", FieldKind::Text), ("N", FieldKind::Number)] {
        schema = apply(
            &mut provider,
            SchemaChange::CreateField {
                table_id: table,
                name: name.into(),
                data_type,
                comment: String::new(),
            },
        );
    }
    let g = field(&schema, "G");
    let mut options = g.options.clone().unwrap();
    options.storage.global = true;
    options.storage.indexing = Indexing::None;
    schema = apply(
        &mut provider,
        SchemaChange::SetFieldOptions {
            table_id: table,
            field_id: g.id,
            options: Box::new(options),
        },
    );
    for (name, kind, of) in [
        ("SumN", SummaryKind::Total, "N"),
        ("Names", SummaryKind::Count, "First Name"),
    ] {
        let of = field(&schema, of).id;
        schema = apply(
            &mut provider,
            SchemaChange::CreateSummaryField {
                table_id: table,
                name: name.into(),
                comment: String::new(),
                summary: SummaryDef::new(kind, of),
            },
        );
    }
    for (name, formula, result) in [
        ("Greeting", "First Name & G", FieldKind::Text),
        ("Tagged", "G & \"x\"", FieldKind::Text),
        ("Share", "N / SumN", FieldKind::Number),
        ("Counted", "Names", FieldKind::Number),
    ] {
        apply(
            &mut provider,
            SchemaChange::CreateCalculationField {
                table_id: table,
                name: name.into(),
                comment: String::new(),
                calculation: CalculationField {
                    result,
                    ..CalculationField::new(formula, context)
                },
            },
        );
    }
    provider.commit_schema_changes().unwrap();
    provider.close();

    let mut provider = open();
    let schema = provider.database_schema().unwrap();
    for name in ["Greeting", "Tagged", "Share", "Counted"] {
        let f = field(&schema, name);
        let text = schema.field_column_text(&contacts(&schema), &f, FieldColumn::Options);
        println!("{name}: {text}");
        assert!(f.unstored, "{name} is unstored: {text}");
    }
    provider.close();
}

/// Auto-entered and validation formulas over a global field made through
/// the local-file path (#187), for FileMaker Pro to check afterwards
/// (schema-path.md 5.17): in a copy of the Contacts starter solution, the
/// global text field G and runs o01-o04's four formulas over it, A1
/// auto-entered `G`, A2 auto-entered `First Name & G`, V1 validated by
/// `Self <> G`, V2 validated by `First Name = G`; saved. A second session
/// reads them back, sets G to "x", makes the record Ada (A1 "x", A2
/// "Adax"), and has V1 "x" refused and V1 "y" written.
#[test]
#[ignore = "requires OM_OPTION_GLOBALS_FILE, a writable copy of the Contacts starter solution (Admin, no password)"]
fn option_formulas_over_a_global_are_made_in_a_copy_of_a_filemaker_file() {
    use om_data::field_options::{
        AutoCalculation, AutoEnterValue, Indexing, OptionFormula, ValidationCalculation,
    };
    use om_data::{DatabaseSchema, FieldColumn, FieldKind, SchemaChange, SchemaField, SchemaTable};
    let path = PathBuf::from(std::env::var_os("OM_OPTION_GLOBALS_FILE").expect("a file"));
    let open = || {
        let mut provider = clean_client();
        provider
            .open_local(&local(&path, Some(("Admin", ""))))
            .unwrap();
        provider
    };
    let contacts = |schema: &DatabaseSchema| -> SchemaTable {
        schema
            .tables
            .iter()
            .find(|t| t.name == "Contacts")
            .unwrap()
            .clone()
    };
    let field = |schema: &DatabaseSchema, name: &str| -> SchemaField {
        contacts(schema)
            .fields
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("no field {name}"))
            .clone()
    };
    let apply = |provider: &mut NativeProvider, change: SchemaChange| {
        provider
            .apply_schema_change(change)
            .unwrap_or_else(|e| panic!("{}", e.message))
    };

    let mut provider = open();
    let mut schema = provider.begin_schema_edit().unwrap();
    let table = contacts(&schema).id;
    let context = schema
        .occurrences
        .iter()
        .find(|o| o.name == "Contacts")
        .unwrap()
        .id;
    for name in ["G", "A1", "A2", "V1", "V2"] {
        schema = apply(
            &mut provider,
            SchemaChange::CreateField {
                table_id: table,
                name: name.into(),
                data_type: FieldKind::Text,
                comment: String::new(),
            },
        );
    }
    let g = field(&schema, "G");
    let mut options = g.options.clone().unwrap();
    options.storage.global = true;
    options.storage.indexing = Indexing::None;
    schema = apply(
        &mut provider,
        SchemaChange::SetFieldOptions {
            table_id: table,
            field_id: g.id,
            options: Box::new(options),
        },
    );
    for (name, text, auto) in [
        ("A1", "G", true),
        ("A2", "First Name & G", true),
        ("V1", "Self <> G", false),
        ("V2", "First Name = G", false),
    ] {
        let f = field(&schema, name);
        let mut options = f.options.clone().unwrap();
        let formula = OptionFormula::new(text, context);
        if auto {
            options.auto_enter.value = AutoEnterValue::Calculated(AutoCalculation {
                formula,
                replace_existing: false,
                skip_when_empty: true,
            });
        } else {
            options.validation.calculation = Some(ValidationCalculation {
                formula,
                only_if_modified: true,
            });
        }
        schema = apply(
            &mut provider,
            SchemaChange::SetFieldOptions {
                table_id: table,
                field_id: f.id,
                options: Box::new(options),
            },
        );
    }
    provider.commit_schema_changes().unwrap();
    provider.close();

    let mut provider = open();
    let schema = provider.database_schema().unwrap();
    for name in ["G", "A1", "A2", "V1", "V2"] {
        let f = field(&schema, name);
        let text = schema.field_column_text(&contacts(&schema), &f, FieldColumn::Options);
        println!("{name}: {text}");
    }
    let g = field(&schema, "G").id;
    let ids: Vec<u64> = ["A1", "A2", "V1"]
        .iter()
        .map(|n| field(&schema, n).id)
        .collect();
    provider.set_global(table, g, 1, "x").unwrap();
    let details = layout(&mut provider, "Contact Details");
    let record = provider
        .create_record(
            details,
            FieldChanges::from([("First Name".to_owned(), Some("Ada".to_owned()))]),
        )
        .unwrap();
    let id = u64::try_from(record.id).unwrap();
    let values = |provider: &mut NativeProvider| -> Vec<String> {
        let need = om_data::host::CalcNeed::Record { table, record: id };
        let data = provider.calc_data(&[need]).unwrap();
        let values = data.records[&(table, id)].clone().unwrap();
        ids.iter()
            .map(|f| match values.get(*f, 1) {
                Some(om_data::calc::Value::Text(t)) => t.as_str().to_owned(),
                Some(other) => format!("{other:?}"),
                None => String::new(),
            })
            .collect()
    };
    assert_eq!(values(&mut provider)[..2], ["x", "Adax"]);
    let v1 = |provider: &mut NativeProvider, text: &str| {
        provider.update_record(
            details,
            record.id,
            FieldChanges::from([("V1".to_owned(), Some(text.to_owned()))]),
        )
    };
    let refused = v1(&mut provider, "x").unwrap_err();
    println!("V1 \"x\": {}", refused.message);
    v1(&mut provider, "y").unwrap();
    assert_eq!(values(&mut provider)[2], "y");
    provider.close();
}

/// Duplicate of Assets' lookup Assignments::Name through the local-file
/// path (#432), for FileMaker's own tools to check afterwards
/// (schema-path.md 5.14): the copy "Name Copy" is saved, and a second
/// session reads it back with the original's options, its lookup
/// included.
#[test]
#[ignore = "requires OM_DUPLICATE_LOOKUP_FILE, a writable copy of the Assets starter solution (Admin, no password)"]
fn a_lookup_field_is_duplicated_in_a_copy_of_a_filemaker_file() {
    use om_data::{DatabaseSchema, FieldColumn, SchemaChange, SchemaField, SchemaTable};
    let path = PathBuf::from(std::env::var_os("OM_DUPLICATE_LOOKUP_FILE").expect("a file"));
    let open = || {
        let mut provider = clean_client();
        provider
            .open_local(&local(&path, Some(("Admin", ""))))
            .unwrap();
        provider
    };
    let assignments = |schema: &DatabaseSchema| -> SchemaTable {
        schema
            .tables
            .iter()
            .find(|t| t.name == "Assignments")
            .unwrap()
            .clone()
    };
    let field = |schema: &DatabaseSchema, name: &str| -> SchemaField {
        assignments(schema)
            .fields
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("no field {name}"))
            .clone()
    };
    let mut provider = open();
    let schema = provider.begin_schema_edit().unwrap();
    let name = field(&schema, "Name");
    provider
        .apply_schema_change(SchemaChange::DuplicateField {
            table_id: assignments(&schema).id,
            field_id: name.id,
        })
        .unwrap_or_else(|e| panic!("{}", e.message));
    provider.commit_schema_changes().unwrap();
    provider.close();

    let mut provider = open();
    let schema = provider.database_schema().unwrap();
    let table = assignments(&schema);
    let (original, copy) = (field(&schema, "Name"), field(&schema, "Name Copy"));
    for f in [&original, &copy] {
        let text = schema.field_column_text(&table, f, FieldColumn::Options);
        println!("{}: {text}", f.name);
    }
    assert_eq!(copy.options, original.options);
    provider.close();
}

/// FileMaker's side of #441: a copy of a starter solution in which
/// OpenMaker changed a text field holding five values to number, and its
/// twin holding the same values, unchanged, for FileMaker Pro to change;
/// with a README saying what to compare. The field is the first stored
/// text field Browse may change on the first layout of a table.
#[test]
#[ignore = "requires OM_TYPE_CHANGE_SOURCE (a starter's .fmp12, Admin with no password) and OM_TYPE_CHANGE_FILES (an output folder)"]
fn type_change_files() {
    use om_data::{FieldKind, SchemaChange};
    let (Some(source), Some(out)) = (
        std::env::var_os("OM_TYPE_CHANGE_SOURCE").map(PathBuf::from),
        std::env::var_os("OM_TYPE_CHANGE_FILES").map(PathBuf::from),
    ) else {
        eprintln!("type_change_files: set OM_TYPE_CHANGE_SOURCE and OM_TYPE_CHANGE_FILES");
        return;
    };
    std::fs::create_dir_all(&out).unwrap();
    let values = ["10", "9", "100", "2.50", "-3"];
    let mut found = String::new();
    for (file, change) in [
        ("Changed by OpenMaker.fmp12", true),
        ("Twin for FileMaker Pro.fmp12", false),
    ] {
        let path = out.join(file);
        let _ = std::fs::remove_file(&path);
        std::fs::copy(&source, &path).unwrap();
        let mut p = clean_client();
        p.open_local(&local(&path, Some(("Admin", "")))).unwrap();
        let schema = p.calc_schema().unwrap().unwrap();
        let (layout, layout_name, table) = schema
            .layouts()
            .iter()
            .find_map(|l| Some((l.id, l.name.clone(), schema.table_of(l.occurrence?)?.id)))
            .unwrap();
        let capabilities = p.record_capabilities(layout).unwrap();
        let definitions = p.database_schema().unwrap();
        let fields = &definitions.table(table).unwrap().fields;
        let shown = p.records(layout, 0, 1).unwrap().fields;
        let (field, field_name) = shown
            .iter()
            .filter(|f| f.kind == FieldKind::Text && capabilities.can_edit(f))
            .find_map(|f| {
                let d = fields.iter().find(|d| d.name == f.name)?;
                let plain = d.kind == om_data::SchemaFieldKind::Normal
                    && d.options.as_ref().is_some_and(|o| {
                        !o.storage.global
                            && o.auto_enter.value == om_data::field_options::AutoEnterValue::None
                    });
                plain.then(|| (d.id, d.name.clone()))
            })
            .unwrap();
        for value in values {
            p.create_record(
                layout,
                FieldChanges::from([(field_name.clone(), Some(value.to_owned()))]),
            )
            .unwrap();
        }
        if change {
            p.begin_schema_edit().unwrap();
            p.apply_schema_change(SchemaChange::ChangeField {
                table_id: table,
                field_id: field,
                name: field_name.clone(),
                data_type: FieldKind::Number,
                comment: String::new(),
            })
            .unwrap();
            p.commit_schema_changes().unwrap();
        }
        p.close();
        // Read back before it is relayed: the type, and the values as typed.
        let mut again = clean_client();
        again
            .open_local(&local(&path, Some(("Admin", ""))))
            .unwrap();
        let read = again.database_schema().unwrap();
        let kind = read
            .table(table)
            .and_then(|t| t.fields.iter().find(|f| f.id == field))
            .map(|f| f.data_type);
        let expected = if change {
            FieldKind::Number
        } else {
            FieldKind::Text
        };
        assert_eq!(kind, Some(expected), "{file}");
        let page = again.records(layout, 0, 10_000).unwrap();
        let column = page
            .fields
            .iter()
            .position(|f| f.name == field_name)
            .unwrap();
        let stored: Vec<&str> = page
            .records
            .iter()
            .filter_map(|r| r.values[column].as_deref())
            .filter(|v| values.contains(v))
            .collect();
        assert_eq!(stored, values, "{file}");
        again.close();
        found = format!("layout “{layout_name}”, field “{field_name}”");
    }
    let readme = format!(
        "# #441: a text field holding values changed to number\n\n\
         Both files are copies of FileMaker Pro 26's starter `{source}` with five new records whose {found} holds, \
         typed as text: {values}.\n\n\
         - `Changed by OpenMaker.fmp12`: OpenMaker's Manage Database changed the field to \
         Number. Open it in FileMaker Pro: Manage Database shows the field as Number; the five \
         values read as typed; sorting by the field ascending gives -3, 2.50, 9, 10, 100; \
         Find 10 finds one record.\n\
         - `Twin for FileMaker Pro.fmp12`: the same values, the field still Text. Change the \
         field to Number in FileMaker Pro's Manage Database (note the words of its question), \
         then read the same and compare the two files.\n",
        source = source
            .iter()
            .rev()
            .take(2)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<PathBuf>()
            .display(),
        values = values.join(", "),
    );
    std::fs::write(out.join("README.md"), readme).unwrap();
}

/// The value-list catalog `[21]` with Content Management's Type list as
/// FileMaker Pro 26.0.3 stores it: its definition (a custom list), values
/// and name, and the catalog's index.
fn add_type_list(root: &mut om_hbam::OwnedNode, values: &str) {
    use om_hbam::OwnedNode;
    let definition = vec![
        0x00, 0x00, 0x00, 0x02, 0x01, 0x00, 0x02, 0x01, 0x00, 0x00, 0x01, 0x02, 0x01, 0x00, 0x02,
        0x01, 0x00, 0x00, 0x01, 0x02, 0x01, 0x00, 0x02, 0x01, 0x00, 0x02, 0x01, 0x00, 0x01, 0x7a,
    ];
    root.put(
        &[&[0x21], &[1]],
        OwnedNode::list([1], vec![OwnedNode::leaf([4], [0, 0, 0, 1])]),
    );
    root.put(
        &[&[0x21], &[5], &[1]],
        OwnedNode::list(
            [1],
            vec![
                OwnedNode::leaf([2], definition),
                om_scripts::text_node(&[3], values),
                om_scripts::text_node(&[0x10], "Type"),
            ],
        ),
    );
}

/// Edit Value List (#869): the values a pop-up menu's Edit… saves are
/// written through the local host as FileMaker Pro 26.0.3 writes them
/// (file-metadata.md 6.5: the member's values `03` and both stamps,
/// nothing else), refused on a revision the list has left, and read back
/// after reopening.
#[test]
fn edit_value_list_saves_a_custom_lists_values() {
    let folder = Folder::new("value-list");
    let stored = "Photo\rImage\rPDF\rAudio\rVideo\r-\rOffice\rWeb\r-";
    let path = folder.file(FILE, PASSWORD, |root| add_type_list(root, stored));
    let before = om_hbam::HbamFile::open(&path).unwrap();
    let before = before.key_tree().unwrap();
    let definition_before = before
        .at(&[0x21, 5, 1, 2])
        .and_then(|n| n.value.map(<[u8]>::to_vec));

    let mut provider = opened(&path);
    let list = provider.custom_value_list(1).unwrap();
    assert_eq!(list.name, "Type");
    assert_eq!(list.values.join("\r"), stored);
    let mut values = list.values.clone();
    values.extend(["OM869 New Value".to_owned(), "-".to_owned()]);
    let set = provider
        .set_value_list_values(om_data::value_lists::ValuesChange {
            list: 1,
            expected: list.revision,
            values: values.clone(),
        })
        .unwrap();
    assert!(set.changed);
    // The revision the dialog opened on is gone: a second OK from it is a
    // conflict, and changes nothing.
    let stale = provider
        .set_value_list_values(om_data::value_lists::ValuesChange {
            list: 1,
            expected: list.revision,
            values: vec!["Lost".into()],
        })
        .unwrap_err();
    assert_eq!(stale.code, Some(306), "{stale:?}");
    // The menus' list, from the schema read again.
    let schema = provider.calc_schema().unwrap().unwrap();
    let menu = schema.value_lists().iter().find(|l| l.id == 1).unwrap();
    assert!(
        matches!(&menu.source, om_data::meta::ValueSource::Custom(v) if *v == values),
        "{:?}",
        menu.source
    );
    provider.close();

    // On disk: the values, the definition as stored, both stamps raised.
    let after = om_hbam::HbamFile::open(&path).unwrap();
    let after = after.key_tree().unwrap();
    let text = |node: Option<&om_hbam::Node<'_>>| {
        node.and_then(|n| n.assembled())
            .and_then(|bytes| om_scsu::decode_scsu(&bytes).ok())
    };
    assert_eq!(
        text(after.at(&[0x21, 5, 1, 3])).as_deref(),
        Some(format!("{stored}\rOM869 New Value\r-").as_str())
    );
    assert_eq!(
        after
            .at(&[0x21, 5, 1, 2])
            .and_then(|n| n.value.map(<[u8]>::to_vec)),
        definition_before
    );
    for stamp in [&[0x21, 5, 1, 0xfc][..], &[0x21, 1, 0xfc]] {
        assert!(after.at(stamp).is_some(), "no stamp at {stamp:x?}");
    }
    let mut reopened = opened(&path);
    assert_eq!(reopened.custom_value_list(1).unwrap().values, values);
}

/// The file FileMaker Pro opens for #869's Edit… (not run by default): a
/// copy of the Content Management starter whose Type list OpenMaker's Edit
/// Value List changed as Pro's twin was changed, a value and a divider
/// added after its last divider, then read back after reopening. Run with
///   OM_VALUE_LIST_SOURCE=<Content_Management/en.fmp12> \
///   OM_VALUE_LIST_FILES=<folder> \
///   cargo test -p om-provider --test local_files value_list_edit_file -- --ignored
#[test]
#[ignore = "requires OM_VALUE_LIST_SOURCE (the Content Management starter, Admin with no password) and OM_VALUE_LIST_FILES (an output folder)"]
fn value_list_edit_file() {
    let (Some(source), Some(out)) = (
        std::env::var_os("OM_VALUE_LIST_SOURCE").map(PathBuf::from),
        std::env::var_os("OM_VALUE_LIST_FILES").map(PathBuf::from),
    ) else {
        eprintln!("value_list_edit_file: set OM_VALUE_LIST_SOURCE and OM_VALUE_LIST_FILES");
        return;
    };
    std::fs::create_dir_all(&out).unwrap();
    let path = out.join("Changed by OpenMaker.fmp12");
    let _ = std::fs::remove_file(&path);
    std::fs::copy(&source, &path).unwrap();
    let mut p = clean_client();
    p.open_local(&local(&path, Some(("Admin", "")))).unwrap();
    let catalog = p.value_list_catalog().unwrap();
    let id = catalog
        .lists
        .iter()
        .find(|l| l.name.as_deref() == Some("Type"))
        .map(|l| l.id)
        .expect("the starter's Type list");
    let list = p.custom_value_list(id).unwrap();
    let mut values = list.values.clone();
    values.extend(["OM869 OpenMaker Value".to_owned(), "-".to_owned()]);
    let set = p
        .set_value_list_values(om_data::value_lists::ValuesChange {
            list: id,
            expected: list.revision,
            values: values.clone(),
        })
        .unwrap();
    assert!(set.changed);
    p.close();
    let mut reopened = clean_client();
    reopened
        .open_local(&local(&path, Some(("Admin", ""))))
        .unwrap();
    assert_eq!(reopened.custom_value_list(id).unwrap().values, values);
    reopened.close();
    let readme = format!(
        "# #869: a value list changed by OpenMaker's Edit Value List\n\n\
         `Changed by OpenMaker.fmp12` is a copy of FileMaker Pro 26's English Content \
         Management starter. OpenMaker's Edit… (Edit Value List) changed its custom value \
         list `Type`, as Pro's twin `Content-after.fmp12` was changed: after the list's last \
         `-` line it added `OM869 OpenMaker Value` and a `-` line. OpenMaker read the values \
         back after reopening.\n\n\
         The list's values now: {}.\n\n\
         To check in FileMaker Pro: open the file (Admin, no password) with no alert; on \
         Content Details, New Record, open the Type pop-up menu. It should show `OM869 \
         OpenMaker Value` after a divider and a divider after it, then Other… and Edit…, \
         with neither `-` line selectable. Type > Edit… should show the same lines. \
         Optionally run Tools > File Maintenance or FMDeveloperTool --checkConsistency.\n",
        values.join(" ¶ ")
    );
    std::fs::write(out.join("README.md"), readme).unwrap();
}
