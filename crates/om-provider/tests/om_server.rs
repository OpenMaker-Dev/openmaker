//! The native provider end to end against an `om-server` on loopback that
//! hosts a temporary copy of test project, over verified TLS. Nothing leaves
//! this machine.
//!
//! `OM_TEST_PROJECT_PASSWORD=... cargo test -p om-provider --test om_server -- --ignored`
//! (the copy's Admin password, which the server checks).

use std::path::PathBuf;

use om_data::{Connection, DataProvider, FieldChanges, RecordPage};
use om_protocol::write::LockReply;
use om_protocol::{LoginEnvironment, MachineInfo};
use om_provider::{NativeConfig, NativeProvider};
use om_server::{HostedFile, Identity, Server, ServerConfig, ServerIdentity};

/// A copy of test project taken with FileMaker Server's backup command, with
/// Contacts records 1 "Test Record", 2 "Hello World", 3 "Ima Computer";
/// git-ignored on the analysis host.
const TEST_PROJECT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../re/work/omserver/testproject-live-1234.fmp12"
);
const FILE: &str = "test project.fmp12";
const ACCOUNT: &str = "Admin";
const PASSWORD_VARIABLE: &str = "OM_TEST_PROJECT_PASSWORD";

/// Admin's password in the copy, from the environment.
fn password() -> String {
    std::env::var(PASSWORD_VARIABLE)
        .unwrap_or_else(|_| panic!("{PASSWORD_VARIABLE} holds the test project's password"))
}

/// A server on an ephemeral loopback port and the CA a client must trust.
/// The hosted copy is removed when this is dropped.
struct Hosted {
    port: u16,
    ca_pem: Vec<u8>,
    copy: PathBuf,
}

impl Drop for Hosted {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.copy);
    }
}

fn host(name: &str) -> Hosted {
    // The server writes every change back to the file it hosts.
    let copy =
        std::env::temp_dir().join(format!("om-provider-{name}-{}.fmp12", std::process::id()));
    std::fs::copy(TEST_PROJECT, &copy).unwrap();
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
            path: copy.clone(),
        }],
        insecure_files: false,
        record_dir: None,
    };
    let server = Server::bind("127.0.0.1:0", config, identity).unwrap();
    // Every download and find is answered through the test view too, and
    // checked against the file engine's view (docs/server-storage.md 5,
    // "Views").
    #[cfg(debug_assertions)]
    let server = server.checking_reads();
    let port = server.local_addr().unwrap().port();
    std::thread::spawn(move || server.serve());
    Hosted { port, ca_pem, copy }
}

fn open(hosted: &Hosted) -> NativeProvider {
    let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
    let mut provider = NativeProvider::new(NativeConfig {
        client_cert_pem: client.cert.pem().into_bytes(),
        client_key_pem: client.key_pair.serialize_pem().into_bytes(),
        additional_ca_pem: hosted.ca_pem.clone(),
        callback_host: "127.0.0.1".into(),
        machine: MachineInfo::default(),
    });
    provider
        .open(&Connection {
            host: "127.0.0.1".into(),
            port: hosted.port,
            file: FILE.into(),
            account: ACCOUNT.into(),
            password: password(),
        })
        .unwrap();
    provider
}

/// The id of the layout named `name`.
fn layout(provider: &mut NativeProvider, name: &str) -> u32 {
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

fn all_records(provider: &mut NativeProvider, layout: u32) -> RecordPage {
    provider.records(layout, 0, 1000).unwrap()
}

#[test]
#[ignore = "requires the git-ignored copy of test project, with its password in OM_TEST_PROJECT_PASSWORD"]
fn pages_records_from_the_record_list() {
    let hosted = host("pages");
    let mut provider = open(&hosted);
    let contacts = layout(&mut provider, "Contact Details");
    let first = provider.records(contacts, 0, 2).unwrap();
    assert_eq!(first.total, 3);
    assert_eq!(
        first.records.iter().map(|r| r.id).collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(text(&first, 1, "First Name"), Some("Test"));
    let rest = provider.records(contacts, 2, 2).unwrap();
    assert_eq!(rest.total, 3);
    assert_eq!(rest.records.iter().map(|r| r.id).collect::<Vec<_>>(), [3]);
    assert_eq!(text(&rest, 3, "First Name"), Some("Ima"));
    assert!(provider.records(contacts, 3, 2).unwrap().records.is_empty());
    provider.close();
}

#[test]
#[ignore = "requires the git-ignored copy of test project, with its password in OM_TEST_PROJECT_PASSWORD"]
fn creates_edits_and_deletes_a_record() {
    let hosted = host("writes");
    let mut provider = open(&hosted);
    let contacts = layout(&mut provider, "Contact Details");
    let caps = provider.record_capabilities(contacts).unwrap();
    assert!(caps.create && caps.update && caps.delete);
    let editable = caps.editable_fields.unwrap();
    assert!(editable.contains("First Name") && !editable.contains("PrimaryKey"));
    let before = all_records(&mut provider, contacts);

    // New Record with a value: the client computes the generated fields.
    let created = provider
        .create_record(
            contacts,
            FieldChanges::from([("First Name".into(), Some("Ada".into()))]),
        )
        .unwrap();
    assert_eq!(created.id, 4, "op71 reserves the id after [T, 00]");
    let page = all_records(&mut provider, contacts);
    assert_eq!(page.total, 4);
    assert_eq!(text(&page, 4, "First Name"), Some("Ada"));
    assert_eq!(text(&page, 4, "Initial"), Some("A"));
    assert_eq!(text(&page, 4, "CreatedBy"), Some(ACCOUNT));
    assert_eq!(text(&page, 4, "ModifiedBy"), Some(ACCOUNT));
    assert_eq!(text(&page, 4, "PrimaryKey").map(str::len), Some(36));
    let created_at = text(&page, 4, "CreationTimestamp").unwrap().to_owned();

    // Edit, then clear: the planner recomputes Initial and the modification
    // fields and keeps the creation fields.
    let edited = provider
        .update_record(
            contacts,
            4,
            FieldChanges::from([("Last Name".into(), Some("Lovelace".into()))]),
        )
        .unwrap();
    assert_eq!(edited.id, 4);
    provider
        .update_record(
            contacts,
            4,
            FieldChanges::from([("First Name".into(), None)]),
        )
        .unwrap();
    let page = all_records(&mut provider, contacts);
    assert_eq!(text(&page, 4, "First Name"), None);
    assert_eq!(text(&page, 4, "Last Name"), Some("Lovelace"));
    assert_eq!(text(&page, 4, "Initial"), Some("L"));
    assert_eq!(text(&page, 4, "CreationTimestamp"), Some(&*created_at));

    // A save that changes nothing leaves the record as it is.
    let unchanged = provider
        .update_record(contacts, 4, FieldChanges::new())
        .unwrap();
    assert_eq!(&unchanged, page.records.iter().find(|r| r.id == 4).unwrap());

    // Delete Record (write-path.md 7).
    provider.delete_record(contacts, 4).unwrap();
    let after = all_records(&mut provider, contacts);
    assert_eq!(after, before, "only the new record changed");
    assert!(provider
        .delete_record(contacts, 4)
        .unwrap_err()
        .to_string()
        .contains("no longer exists"));
    provider.close();

    // The server wrote the file: a new session sees the same records.
    let mut reopened = open(&hosted);
    let contacts = layout(&mut reopened, "Contact Details");
    assert_eq!(all_records(&mut reopened, contacts), before);
    reopened.close();
}

#[test]
#[ignore = "requires the git-ignored copy of test project, with its password in OM_TEST_PROJECT_PASSWORD"]
fn refuses_stale_and_locked_edits() {
    let hosted = host("conflicts");
    let mut a = open(&hosted);
    let mut b = open(&hosted);
    let contacts = layout(&mut a, "Contact Details");
    a.records(contacts, 0, 10).unwrap();
    b.records(contacts, 0, 10).unwrap();

    // B saves first; A still shows the record before B's change.
    let change = |value: &str| FieldChanges::from([("Title".into(), Some(value.into()))]);
    b.update_record(contacts, 2, change("Dr.")).unwrap();
    let error = a.update_record(contacts, 2, change("Ms.")).unwrap_err();
    assert!(
        error.to_string().contains("Another client changed"),
        "{error}"
    );
    // Once A reloads, its save applies.
    a.records(contacts, 0, 10).unwrap();
    a.update_record(contacts, 2, change("Ms.")).unwrap();

    // Another user holding the record's lock blocks saves and deletes and
    // is named (write-path.md 4.2).
    let mut holder = raw_session(&hosted, "holder");
    let path: [&[u8]; 3] = [&[0x80, 0x02], &[0x05], &[0x03]];
    assert!(matches!(
        holder.lock(&path, 7, true).unwrap(),
        LockReply::Granted { .. }
    ));
    let error = a.update_record(contacts, 3, change("Mx.")).unwrap_err();
    assert!(error.to_string().contains("holder (Admin)"), "{error}");
    let error = a.delete_record(contacts, 3).unwrap_err();
    assert!(error.to_string().contains("holder (Admin)"), "{error}");
    holder.kill_all_locks(7).unwrap();
    a.update_record(contacts, 3, change("Mx.")).unwrap();
    let page = all_records(&mut b, contacts);
    assert_eq!(text(&page, 2, "Title"), Some("Ms."));
    assert_eq!(text(&page, 3, "Title"), Some("Mx."));
    holder.disconnect().unwrap();
    a.close();
    b.close();
}

/// A plain session logged in as `name`, for holding locks.
fn raw_session(hosted: &Hosted, name: &str) -> om_session::Session {
    let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
    let mut session = om_session::Session::connect(
        "127.0.0.1",
        hosted.port,
        client.cert.pem().as_bytes(),
        client.key_pair.serialize_pem().as_bytes(),
        "127.0.0.1",
        &MachineInfo {
            machine_name: name.into(),
            ..MachineInfo::default()
        },
    )
    .unwrap();
    session.open_file("127.0.0.1", FILE).unwrap();
    let creation = session.fetch_internal_creation().unwrap();
    let environment = LoginEnvironment::for_remote_file("127.0.0.1", FILE);
    assert!(session
        .login(ACCOUNT, &password(), creation, &environment)
        .unwrap()
        .is_success());
    session
}

#[test]
#[ignore = "requires the git-ignored copy of test project, with its password in OM_TEST_PROJECT_PASSWORD"]
fn finds_omits_and_sorts_the_found_set() {
    use om_data::found::{FindKind, FindRequest, FoundSetChange, SortField, SortOrder};
    let hosted = host("found");
    let mut provider = open(&hosted);
    let contacts = layout(&mut provider, "Contact Details");
    let ids = |provider: &mut NativeProvider| -> Vec<i64> {
        all_records(provider, contacts)
            .records
            .iter()
            .map(|r| r.id)
            .collect()
    };
    let request = |text: &str| {
        let mut request = FindRequest::default();
        request.set("First Name", text);
        request
    };
    // Two requests (op86 SimpleFind each): "h" finds Hello, "i" finds Ima.
    let found = provider
        .change_found_set(
            contacts,
            FoundSetChange::Find {
                requests: vec![request("h"), request("i")],
                kind: FindKind::Replace,
            },
        )
        .unwrap();
    assert_eq!((found.found, found.total, found.sorted), (2, 3, false));
    assert_eq!(ids(&mut provider), [2, 3]);
    // Pages follow the sorted order.
    let sorted = provider
        .change_found_set(
            contacts,
            FoundSetChange::Sort(vec![SortField::new("First Name", SortOrder::Descending)]),
        )
        .unwrap();
    assert!(sorted.sorted);
    assert_eq!(ids(&mut provider), [3, 2]);
    let page = provider.records(contacts, 0, 1).unwrap();
    assert_eq!((page.total, page.records[0].id), (2, 3));
    // Omit Ima; the rest are the omitted ones.
    provider
        .change_found_set(contacts, FoundSetChange::Omit(vec![3]))
        .unwrap();
    assert_eq!(ids(&mut provider), [2]);
    provider
        .change_found_set(contacts, FoundSetChange::ShowOmitted)
        .unwrap();
    assert_eq!(ids(&mut provider), [1, 3]);
    // A leading omit request in Constrain: from the found set.
    let mut omit = request("test");
    omit.omit = true;
    provider
        .change_found_set(
            contacts,
            FoundSetChange::Find {
                requests: vec![omit],
                kind: FindKind::Constrain,
            },
        )
        .unwrap();
    assert_eq!(ids(&mut provider), [3]);
    let none = provider.change_found_set(
        contacts,
        FoundSetChange::Find {
            requests: vec![request("zzz")],
            kind: FindKind::Replace,
        },
    );
    assert_eq!(none.unwrap_err().code, Some(401));
    assert!(ids(&mut provider).is_empty());
    provider
        .change_found_set(contacts, FoundSetChange::ShowAll)
        .unwrap();
    assert_eq!(ids(&mut provider), [1, 2, 3]);
    provider.close();
}

fn change(field: &str, value: &str) -> FieldChanges {
    FieldChanges::from([(field.into(), Some(value.into()))])
}

fn ids_of(page: &RecordPage) -> Vec<i64> {
    page.records.iter().map(|r| r.id).collect()
}

#[test]
#[ignore = "requires the git-ignored copy of test project, with its password in OM_TEST_PROJECT_PASSWORD"]
fn transactions_hold_changes_and_locks_until_commit() {
    use om_data::runtime::TransactionOptions;
    let hosted = host("transaction");
    let mut a = open(&hosted);
    let mut b = open(&hosted);
    let contacts = layout(&mut a, "Contact Details");
    let before = all_records(&mut b, contacts);
    all_records(&mut a, contacts);

    a.begin_transaction(TransactionOptions::default()).unwrap();
    a.update_record(contacts, 2, change("Title", "Dr."))
        .unwrap();
    a.update_record(contacts, 2, change("Company", "Acme"))
        .unwrap();
    let created = a
        .create_record(contacts, change("First Name", "Txn"))
        .unwrap();
    assert_eq!(created.id, 4, "op71 reserved the id at New Record");
    a.delete_record(contacts, 3).unwrap();
    // The transaction's window sees its changes; the host has none yet.
    let seen = all_records(&mut a, contacts);
    assert_eq!(ids_of(&seen), [1, 2, 4]);
    assert_eq!(text(&seen, 2, "Title"), Some("Dr."));
    assert_eq!(text(&seen, 2, "Company"), Some("Acme"));
    assert_eq!(text(&seen, 4, "First Name"), Some("Txn"));
    assert_eq!(all_records(&mut b, contacts), before);
    // The changed and deleted records are locked by the transaction.
    let locked = b
        .update_record(contacts, 2, change("Title", "Mx."))
        .unwrap_err();
    assert_eq!(locked.code, Some(301), "{locked}");
    assert_eq!(b.delete_record(contacts, 3).unwrap_err().code, Some(301));

    a.commit_transaction(false).unwrap();
    let after = all_records(&mut b, contacts);
    assert_eq!(ids_of(&after), [1, 2, 4]);
    assert_eq!(text(&after, 2, "Title"), Some("Dr."));
    assert_eq!(text(&after, 2, "Company"), Some("Acme"));
    // Auto-enter and stored calculations ran at commit.
    assert_eq!(text(&after, 4, "First Name"), Some("Txn"));
    assert_eq!(text(&after, 4, "Initial"), Some("T"));
    assert_eq!(text(&after, 4, "CreatedBy"), Some(ACCOUNT));
    assert_eq!(text(&after, 4, "PrimaryKey").map(str::len), Some(36));
    // The locks are released.
    b.update_record(contacts, 2, change("Title", "Mx."))
        .unwrap();
    a.close();
    b.close();
}

#[test]
#[ignore = "requires the git-ignored copy of test project, with its password in OM_TEST_PROJECT_PASSWORD"]
fn reverted_transactions_write_nothing_and_release_their_locks() {
    use om_data::runtime::TransactionOptions;
    let hosted = host("revert");
    let mut a = open(&hosted);
    let mut b = open(&hosted);
    let contacts = layout(&mut a, "Contact Details");
    let before = all_records(&mut a, contacts);
    all_records(&mut b, contacts);

    a.begin_transaction(TransactionOptions::default()).unwrap();
    a.update_record(contacts, 1, change("Title", "Sir"))
        .unwrap();
    a.create_record(contacts, change("First Name", "Gone"))
        .unwrap();
    a.delete_record(contacts, 2).unwrap();
    a.revert_transaction().unwrap();
    assert_eq!(all_records(&mut a, contacts), before);
    assert_eq!(all_records(&mut b, contacts), before);
    b.update_record(contacts, 1, change("Title", "Sir"))
        .unwrap();

    // A record created and deleted inside a transaction is never written;
    // its reserved id stays used.
    all_records(&mut a, contacts);
    a.begin_transaction(TransactionOptions::default()).unwrap();
    let ghost = a
        .create_record(contacts, change("First Name", "Ghost"))
        .unwrap();
    a.delete_record(contacts, ghost.id).unwrap();
    a.commit_transaction(false).unwrap();
    assert_eq!(ids_of(&all_records(&mut b, contacts)), [1, 2, 3]);
    let next = b
        .create_record(contacts, change("First Name", "Next"))
        .unwrap();
    assert!(next.id > ghost.id, "{} after {}", next.id, ghost.id);
    // Without a transaction there is nothing to commit or revert.
    assert_eq!(a.commit_transaction(false).unwrap_err().code, Some(3));
    assert_eq!(a.revert_transaction().unwrap_err().code, Some(3));
    a.close();
    b.close();
}

/// Contacts for the find operator tests: (First Name, Last Name, Company).
const PEOPLE: [(&str, &str, &str); 24] = [
    ("Chris", "Smith", "Market Services"),
    ("Smith", "Chris", "Marketing Inc"),
    ("Christopher", "Smithson", "Supermarket"),
    ("Gray", "Marten", "Marten and Jones Interiors"),
    ("Grey", "Jones", "Jones and Marten Interiors"),
    ("Jon", "Doe", "Acme, Ltd."),
    ("John", "Doe", "Acme Ltd."),
    ("John Smith", "Doe", "Springville Co"),
    ("Junior", "Roe", "ColdSpring Harbor"),
    ("Stephanie", "Phan", "Johnson & Phillips"),
    ("Phan", "Stephan", "Paulson & Philson"),
    ("Don", "Ron", "Ongoing Market Research"),
    ("Ron", "Bron", "joey@abc.net"),
    ("Bron", "Don", "a   b"),
    ("òpera", "Opera", "x"),
    ("opera", "Òpera", "y"),
    ("3", "30", "300"),
    ("53", "43", "5"),
    ("Sophie", "Steve", "Sven"),
    ("Äpfel", "Zeta", "Business"),
    ("Zeta", "alpha", "Home"),
    ("alpha", "Beta", "Other"),
    ("", "Empty", ""),
    ("Jr.", "Junior", "Ltd"),
];

#[test]
#[ignore = "requires the git-ignored copy of test project, with its password in OM_TEST_PROJECT_PASSWORD"]
fn find_operators_match_the_memory_engine_over_many_records() {
    use om_data::found::{self, FindKind, FindRequest, FoundSetChange};
    let hosted = host("operators");
    let mut provider = open(&hosted);
    let contacts = layout(&mut provider, "Contact Details");
    for (first, last, company) in PEOPLE {
        let mut values = FieldChanges::new();
        values.insert(
            "First Name".into(),
            (!first.is_empty()).then(|| first.into()),
        );
        values.insert("Last Name".into(), Some(last.into()));
        values.insert(
            "Company".into(),
            (!company.is_empty()).then(|| company.into()),
        );
        provider.create_record(contacts, values).unwrap();
    }
    let all = all_records(&mut provider, contacts);
    assert_eq!(all.records.len(), 27);
    let name = |page: &RecordPage, id: i64| text(page, id, "First Name").unwrap_or("").to_owned();
    let mut check = |field: &str, criterion: &str, expected: Option<&[&str]>| {
        let mut request = FindRequest::default();
        request.set(field, criterion);
        let requests = vec![request];
        let host = provider.change_found_set(
            contacts,
            FoundSetChange::Find {
                requests: requests.clone(),
                kind: FindKind::Replace,
            },
        );
        let memory = found::find(&all.fields, &all.records, &requests);
        match (host, memory) {
            (Ok(_), Ok(ids)) => {
                let page = all_records(&mut provider, contacts);
                assert_eq!(ids_of(&page), ids, "{field} {criterion:?}");
                if let Some(expected) = expected {
                    let mut names: Vec<String> = ids.iter().map(|id| name(&all, *id)).collect();
                    names.sort();
                    let mut expected: Vec<String> =
                        expected.iter().map(|s| s.to_string()).collect();
                    expected.sort();
                    assert_eq!(names, expected, "{field} {criterion:?}");
                }
            }
            (Err(h), Err(m)) => assert_eq!(h.code, m.code, "{field} {criterion:?}"),
            (Err(h), Ok(ids)) if ids.is_empty() => {
                assert_eq!(h.code, Some(401), "{field} {criterion:?}")
            }
            (h, m) => panic!("{field} {criterion:?}: host {h:?}, memory {m:?}"),
        }
    };
    // Words, whole words and whole values.
    check("First Name", "Chris", Some(&["Chris", "Christopher"]));
    check("Company", "=Market", Some(&["Chris", "Don"]));
    check("First Name", "==John Smith", Some(&["John Smith"]));
    check("First Name", "==John", Some(&["John"]));
    // Wildcards: one character, one digit, any characters.
    check("First Name", "Gr@y", Some(&["Gray", "Grey"]));
    check("First Name", "@on", Some(&["Jon", "Don", "Ron"]));
    check("First Name", "#", Some(&["3"]));
    check("First Name", "##", Some(&["53"]));
    check("First Name", "#3", Some(&["53"]));
    check("First Name", "Jo*n", Some(&["Jon", "John", "John Smith"]));
    check("First Name", "J*r", Some(&["Junior", "Jr."]));
    check("First Name", "*phan*", Some(&["Stephanie", "Phan"]));
    check(
        "First Name",
        "S*",
        Some(&["Smith", "Stephanie", "Sophie", "John Smith"]),
    );
    // Phrases, punctuation and accents.
    check("Company", "\"Marten and Jones Interiors\"", Some(&["Gray"]));
    check("Company", "\", Ltd.\"", Some(&["Jon"]));
    check("Company", "\"Spring\"", Some(&["John Smith"]));
    check("Company", "*\"son & Phil\"*", Some(&["Stephanie", "Phan"]));
    check("Company", "*\"son & Phillips\"", Some(&["Stephanie"]));
    check("Company", "\"@\"", Some(&["Ron"]));
    check("Company", "joey\\@abc.net", Some(&["Ron"]));
    check("Company", "\"   \"", Some(&["Bron"]));
    check("First Name", "\"òpera\"", Some(&["òpera"]));
    check("First Name", "òpera", Some(&["òpera", "opera"]));
    // Empty, not empty, invalid, duplicates, relaxed.
    check("First Name", "=", Some(&[""]));
    check("First Name", "==", Some(&[""]));
    check("Company", "*", None);
    check("Company", "?", Some(&[]));
    check("Last Name", "!", None);
    check("Last Name", "!Doe", Some(&["Jon", "John", "John Smith"]));
    check("First Name", "~opera", Some(&["òpera", "opera"]));
    // Ranges on the word index.
    check("First Name", "<B", Some(&["alpha", "Äpfel", "3", "53"]));
    check("First Name", "Y...Zz", Some(&["Zeta"]));
    // Timestamps: every record was created today.
    check("CreationTimestamp", "//", None);
    check("CreationTimestamp", "<1/1/2000", Some(&[]));
    check("CreationTimestamp", ">=1/1/2000", None);
    check("CreationTimestamp", "*/*/* AM...PM", None);
    // A criterion the field's type cannot read.
    check("CreationTimestamp", "someday", None);

    // Several requests and omits combine as before, with client filters.
    let mut omit = FindRequest::default();
    omit.set("First Name", "Jo*");
    omit.omit = true;
    let mut find = FindRequest::default();
    find.set("Company", "*Ltd*");
    let requests = vec![find, omit];
    provider
        .change_found_set(
            contacts,
            FoundSetChange::Find {
                requests: requests.clone(),
                kind: FindKind::Replace,
            },
        )
        .unwrap();
    let page = all_records(&mut provider, contacts);
    assert_eq!(
        ids_of(&page),
        found::find(&all.fields, &all.records, &requests).unwrap()
    );
    provider.close();
}

#[test]
#[ignore = "requires the git-ignored copy of test project, with its password in OM_TEST_PROJECT_PASSWORD"]
fn value_lists_index_values_serials_and_language_sorts() {
    use om_data::found::{FoundSetChange, SortField, SortOrder};
    let hosted = host("value-lists");
    let mut provider = open(&hosted);
    let contacts = layout(&mut provider, "Contact Details");
    // The Title list (file-metadata.md 6.1 custom values, dividers kept).
    let titles = provider.value_list(3).unwrap();
    assert_eq!(&titles[..3], ["Mr.", "Mrs.", "Ms."]);
    assert!(titles.contains(&"-".to_owned()));
    for (id, title) in [(1, "Prof."), (2, "Mr."), (3, "Ms.")] {
        provider
            .update_record(contacts, id, change("Title", title))
            .unwrap();
    }
    let by_list = SortField {
        value_list: Some(titles.into_iter().filter(|t| t != "-").collect()),
        ..SortField::new("Title", SortOrder::Ascending)
    };
    provider
        .change_found_set(contacts, FoundSetChange::Sort(vec![by_list]))
        .unwrap();
    assert_eq!(ids_of(&all_records(&mut provider, contacts)), [2, 3, 1]);
    // Swedish puts Ä after Z; the index's own order puts it with A.
    let a_umlaut = provider
        .create_record(contacts, change("First Name", "Äpfel"))
        .unwrap();
    let zeta = provider
        .create_record(contacts, change("First Name", "Zeta"))
        .unwrap();
    let swedish = SortField {
        language: Some(41),
        ..SortField::new("First Name", SortOrder::Ascending)
    };
    provider
        .change_found_set(contacts, FoundSetChange::Sort(vec![swedish]))
        .unwrap();
    let order = ids_of(&all_records(&mut provider, contacts));
    assert_eq!(&order[order.len() - 2..], [zeta.id, a_umlaut.id]);
    provider
        .change_found_set(
            contacts,
            FoundSetChange::Sort(vec![SortField::new("First Name", SortOrder::Ascending)]),
        )
        .unwrap();
    assert_eq!(
        ids_of(&all_records(&mut provider, contacts))[0],
        a_umlaut.id
    );
    // Insert from Index lists the value index's values in index order.
    let values = provider.index_values(contacts, "First Name").unwrap();
    assert_eq!(values, ["Äpfel", "Hello", "Ima", "Test", "Zeta"]);
    // Set Next Serial Value stores the field's next serial (op72).
    let schema = provider.calc_schema().unwrap().unwrap();
    let occurrence = schema
        .occurrences()
        .find(|o| o.name == "Contacts")
        .map(|o| o.id)
        .unwrap();
    provider
        .set_next_serial_value(occurrence, 4, "INV0100")
        .unwrap();
    let mut session = raw_session(&hosted, "serial");
    let read = session
        .get_serial(&[0x80, 0x02], &[0x04], "", 0, 0)
        .unwrap();
    assert_eq!((read.status, read.value.as_str()), (0, "INV0100"));
    session.disconnect().unwrap();
    provider.close();
}

#[test]
#[ignore = "requires the git-ignored copy of test project, with its password in OM_TEST_PROJECT_PASSWORD"]
fn related_field_criteria_join_back_to_the_layouts_table() {
    use om_data::found::{FindKind, FindRequest, FoundSetChange};
    use om_hbam::{serial, OwnedNode};
    let hosted = host("related");
    let mut provider = open(&hosted);
    let contacts = layout(&mut provider, "Contact Details");
    let page = all_records(&mut provider, contacts);
    let primary_key = |id| text(&page, id, "PrimaryKey").unwrap().to_owned();
    let schema = provider.calc_schema().unwrap().unwrap();
    let phones = schema
        .occurrences()
        .find(|o| o.name == "Phone Numbers")
        .unwrap()
        .id;
    let table = schema.table_of(phones).unwrap();
    let field = |name: &str| table.fields.values().find(|f| f.name == name).unwrap().id;
    let (number, foreign_key) = (field("Number"), field("ForeignKey"));
    let key_bytes = |v: u64| {
        let bytes = v.to_be_bytes();
        bytes[bytes.iter().position(|b| *b != 0).unwrap_or(7)..].to_vec()
    };
    // Phone numbers of Hello World (2) and Ima Computer (3), written as
    // FileMaker Pro writes records (write-path.md 1.1).
    let mut session = raw_session(&hosted, "phones");
    let table_key = key_bytes(table.id);
    for (txn, (owner, phone)) in [(2, "555-1234"), (3, "555-9999"), (3, "777")]
        .into_iter()
        .enumerate()
    {
        let txn = txn as u32 + 100;
        let reply = session.create_record(&table_key, txn).unwrap();
        let id = serial::decode(&reply.record[0]).unwrap();
        let mut cells = vec![
            OwnedNode::leaf(key_bytes(number), om_scsu::encode_scsu(phone)),
            OwnedNode::leaf(
                key_bytes(foreign_key),
                om_scsu::encode_scsu(&primary_key(owner)),
            ),
        ];
        cells.sort_by(|a, b| a.segment.cmp(&b.segment));
        let record = OwnedNode::list(serial::encode(id), cells);
        session.upload_records(&table_key, vec![record]).unwrap();
        session
            .commit_records(
                &table_key,
                vec![OwnedNode::leaf(serial::encode(id), vec![1])],
                txn,
            )
            .unwrap();
        session.kill_all_locks(txn).unwrap();
    }
    session.disconnect().unwrap();
    let mut find = |criteria: &[(&str, &str)]| -> Result<Vec<i64>, Option<i32>> {
        let mut request = FindRequest::default();
        for (field, text) in criteria {
            request.set(field, *text);
        }
        provider
            .change_found_set(
                contacts,
                FoundSetChange::Find {
                    requests: vec![request],
                    kind: FindKind::Replace,
                },
            )
            .map_err(|e| e.code)?;
        Ok(ids_of(&all_records(&mut provider, contacts)))
    };
    assert_eq!(find(&[("Phone Numbers::Number", "555-1234")]), Ok(vec![2]));
    assert_eq!(find(&[("Phone Numbers::Number", "555*")]), Ok(vec![2, 3]));
    assert_eq!(find(&[("Phone Numbers::Number", "=777")]), Ok(vec![3]));
    assert_eq!(
        find(&[("Phone Numbers::Number", "555*"), ("First Name", "Hello")]),
        Ok(vec![2])
    );
    // Contacts without a matching phone number, and none at all.
    assert_eq!(find(&[("Phone Numbers::Number", "888")]), Err(Some(401)));
    assert_eq!(find(&[("Email Addresses::Address", "*")]), Err(Some(401)));
    assert_eq!(find(&[("Nowhere::Number", "1")]), Err(Some(102)));
    provider.close();
}
