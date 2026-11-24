//! A local file hosted privately for the client that opened it
//! ([`LocalHost`]): logins checked against the passwords the file stores
//! and admitted only for active FileMaker-file accounts with no password
//! change due in the built-in [Full Access] set; only the client's
//! certificate accepted; one open of a file at a time; deferred work saved
//! only once an admitted account logs in; read-only files refused at the
//! write requests; a connection stalled in its handshake closed with the
//! host; and files it cannot read refused without being changed. The file
//! is the synthetic Tasks file of `fixtures/tasks.rs`, with local accounts.

#[path = "fixtures/tasks.rs"]
mod fixture;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use fixture::{
    ACCOUNT, CLERK, CLERK_PASSWORD, FILE, FORMER, FORMER_PASSWORD, INTERNAL_CREATION, NEWCOMER,
    NEWCOMER_PASSWORD, PASSWORD, TABLE,
};
use om_hbam::slot::uint_segment;
use om_hbam::OwnedNode;
use om_protocol::{LoginEnvironment, MachineInfo};
use om_server::{
    LocalHost, LocalHostError, ReadOnly, LOGIN_INACTIVE_ACCOUNT, LOGIN_PASSWORD_CHANGE_DUE,
    LOGIN_UNREADABLE_ACCOUNT, LOGIN_UNSUPPORTED_PRIVILEGES,
};
use om_session::Session;

/// A folder of its own for each test, removed when dropped.
struct Folder(PathBuf);

impl Folder {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("om-local-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    /// The Tasks file with local accounts, as `name` in this folder, after
    /// `change`.
    fn tasks_with(&self, name: &str, change: impl FnOnce(&mut OwnedNode)) -> PathBuf {
        let mut root = fixture::tree(&[], true);
        fixture::add_local_accounts(&mut root);
        change(&mut root);
        let path = self.0.join(name);
        fixture::write_tree(&path, &root);
        path
    }

    fn tasks(&self, name: &str) -> PathBuf {
        self.tasks_with(name, |_| {})
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

struct Client {
    cert: Vec<u8>,
    key: Vec<u8>,
}

fn client() -> Client {
    let generated = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
    Client {
        cert: generated.cert.pem().into_bytes(),
        key: generated.signing_key.serialize_pem().into_bytes(),
    }
}

fn connect(host: &LocalHost, client: &Client) -> Result<Session, om_session::SessionError> {
    Session::connect_verified(
        "127.0.0.1",
        host.port(),
        &client.cert,
        &client.key,
        "127.0.0.1",
        &MachineInfo::default(),
        host.certificate_pem(),
    )
}

/// A session logged in to the host's file as `account`, or the Login
/// reply's error code.
fn logged_in(
    host: &LocalHost,
    client: &Client,
    account: &str,
    password: &str,
) -> Result<Session, u16> {
    let mut session = connect(host, client).unwrap();
    session.open_file("127.0.0.1", host.file_name()).unwrap();
    assert_eq!(
        session.fetch_internal_creation().unwrap(),
        INTERNAL_CREATION
    );
    let environment = LoginEnvironment::for_remote_file("127.0.0.1", host.file_name());
    let reply = session
        .login(account, password, INTERNAL_CREATION, &environment)
        .unwrap();
    if reply.error_code == 0 {
        Ok(session)
    } else {
        session.disconnect().unwrap();
        Err(reply.error_code)
    }
}

/// Log in to the host's file as `account`: the Login reply's error code.
fn login(host: &LocalHost, client: &Client, account: &str, password: &str) -> u16 {
    match logged_in(host, client, account, password) {
        Ok(session) => {
            session.disconnect().unwrap();
            0
        }
        Err(code) => code,
    }
}

/// Account record `record`'s status (`[23, 1, 5, A, 0a]`).
fn set_status(root: &mut OwnedNode, record: u8, status: Option<&[u8]>) {
    let path: [&[u8]; 5] = [&[23], &[1], &[5], &[record], &[0x0a]];
    match status {
        Some(value) => root.put(&path, OwnedNode::leaf(Vec::new(), value.to_vec())),
        None => {
            root.remove(&path);
        }
    }
}

#[test]
fn only_verified_accounts_log_in_and_refusals_leave_the_file_unchanged() {
    let folder = Folder::new("logins");
    let path = folder.tasks(FILE);
    let before = std::fs::read(&path).unwrap();
    let client = client();
    let host = LocalHost::open(&path, &client.cert).unwrap();
    assert_eq!(host.file_name(), FILE);
    assert_eq!(host.read_only(), None);
    assert_eq!(host.suggested_account().as_deref(), Some(ACCOUNT));
    // Wrong passwords, unknown accounts and [Guest] are wrong credentials.
    for (account, password) in [
        (ACCOUNT, ""),
        (ACCOUNT, "Correct horse"),
        (ACCOUNT, CLERK_PASSWORD),
        (CLERK, PASSWORD),
        ("Nobody", ""),
        ("[Guest]", ""),
    ] {
        assert_eq!(
            login(&host, &client, account, password),
            212,
            "{account:?} with {password:?}"
        );
    }
    // Clerk's password is right, but its set 2 lacks the layout, script,
    // value list and other privileges of the built-in [Data Entry Only]
    // (session-identity.md 2): not the verified set, so refused.
    assert_eq!(
        login(&host, &client, CLERK, CLERK_PASSWORD),
        LOGIN_UNSUPPORTED_PRIVILEGES
    );
    // Its user, having mistyped six times, is not held back: a local
    // file's host has no login limits.
    assert_eq!(login(&host, &client, ACCOUNT, PASSWORD), 0);
    assert!(host.close());
    assert_eq!(std::fs::read(&path).unwrap(), before);

    let host = LocalHost::open(&path, &client.cert).unwrap();
    // Names match in any case, passwords exactly.
    assert_eq!(login(&host, &client, "admin", PASSWORD), 0);
    assert!(host.close());
    assert_eq!(folder.files(), [FILE]);
}

#[test]
fn accounts_log_in_only_while_active_with_no_password_change_due() {
    let folder = Folder::new("states");
    let client = client();
    // Active: the starter solutions' Admin (its password set 2020-04-03)
    // and an account without a status, which FileMaker reads as active.
    let starter = [0x08, 0x9f, 0x91, 0x03, 0x80, 0x05, 0x52, 0x17, 0x30];
    for (tag, status) in [("starter", Some(&starter[..])), ("absent", None)] {
        let path = folder.tasks_with(&format!("{tag}.fmp12"), |root| {
            set_status(root, 2, status);
        });
        let host = LocalHost::open(&path, &client.cert).unwrap();
        assert_eq!(login(&host, &client, ACCOUNT, PASSWORD), 0, "{tag}");
        assert!(host.close());
    }
    // An inactive account and one whose password must be changed first:
    // FileMaker's refusals, after the password, which alone is still just
    // wrong; the file is left as it was.
    let path = folder.tasks(FILE);
    let before = std::fs::read(&path).unwrap();
    let host = LocalHost::open(&path, &client.cert).unwrap();
    assert_eq!(
        login(&host, &client, FORMER, FORMER_PASSWORD),
        LOGIN_INACTIVE_ACCOUNT
    );
    assert_eq!(login(&host, &client, FORMER, PASSWORD), 212);
    assert_eq!(
        login(&host, &client, NEWCOMER, NEWCOMER_PASSWORD),
        LOGIN_PASSWORD_CHANGE_DUE
    );
    assert!(host.close());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

/// An account whose password must be changed sets a new one as
/// FileMaker's `SetPassword` does (local-files.md 2.4): refused for a wrong
/// old password (212) and an inactive account (210, whatever the old
/// password, as FileMaker checks it first); then it logs in with the new
/// password, one outside ASCII, and not the old, also after the file is
/// opened again, and the saved record's checksum `04` is FileMaker's over
/// what it now holds. (It is in [Full Access], which skips `SetPassword`'s
/// other checks.)
#[test]
fn an_account_due_a_change_sets_a_new_password_and_logs_in_with_it() {
    use om_server::hosting::PasswordChangeError;
    // `é` and Greek `έ`, which FileMaker decomposes to `ε` U+030D, not
    // Unicode's U+0301.
    const NEW: &str = "fr\u{e9}sh secret \u{3ad}";
    let folder = Folder::new("change");
    let client = client();
    let path = folder.tasks(FILE);
    let host = LocalHost::open(&path, &client.cert).unwrap();
    assert_eq!(
        host.change_password(NEWCOMER, "not it", NEW),
        Err(PasswordChangeError::Refused)
    );
    // An inactive account (210): nothing written.
    for old in [FORMER_PASSWORD, "not it"] {
        assert_eq!(
            host.change_password(FORMER, old, NEW),
            Err(PasswordChangeError::Inactive)
        );
    }
    assert_eq!(login(&host, &client, NEWCOMER, NEWCOMER_PASSWORD), 211);
    host.change_password(NEWCOMER, NEWCOMER_PASSWORD, NEW)
        .unwrap();
    assert_eq!(login(&host, &client, NEWCOMER, NEW), 0);
    assert_eq!(login(&host, &client, NEWCOMER, NEWCOMER_PASSWORD), 212);
    assert!(host.close());
    let host = LocalHost::open(&path, &client.cert).unwrap();
    assert_eq!(login(&host, &client, NEWCOMER, NEW), 0);
    assert!(host.close());

    let file = om_hbam::HbamFile::open(&path).unwrap();
    let value = |path: &[&[u8]]| {
        file.subtree(path)
            .unwrap()
            .and_then(|node| node.value.map(<[u8]>::to_vec))
    };
    let account = |key: u8| value(&[&[23], &[1], &[5], &[5], &[key]]);
    let status = account(0x0a).unwrap();
    let decoded = om_server::accounts::AccountStatus::decode(Some(status.as_slice())).unwrap();
    assert!(decoded.is_active() && !decoded.must_change_password());
    let password = account(6).unwrap();
    let version = value(&[&[2], &[5]]).map(|v| u16::from_be_bytes([v[0], v[1]]));
    let uuid = value(&[&[23], &[4], &[1], &[0x10]]);
    let creation = value(&[&[2], &[254]]);
    let set = account(0x0b);
    let checksum = om_server::accounts::checksum(&om_server::accounts::ChecksumInput {
        version,
        access_uuid: uuid.as_deref(),
        creation: creation.as_deref(),
        key: &[5],
        kind: account(2).and_then(|v| v.first().copied()),
        name: NEWCOMER,
        password: &password,
        description: None,
        status: Some(&status),
        set: set.as_deref(),
    });
    assert_eq!(account(4).unwrap(), checksum.to_vec());
}

/// An account outside [Full Access] gets `SetPassword`'s 209 for a new
/// password that is its current one, also in another form FileMaker
/// decomposes alike: it would log in the same.
#[test]
fn a_password_that_hashes_as_the_current_one_is_unchanged() {
    use om_server::hosting::PasswordChangeError;
    let folder = Folder::new("unchanged");
    let client = client();
    let path = folder.tasks_with(FILE, fixture::put_data_entry_only);
    let host = LocalHost::open(&path, &client.cert).unwrap();
    host.change_password(CLERK, CLERK_PASSWORD, "caf\u{e9}")
        .unwrap();
    for same in ["caf\u{e9}", "cafe\u{301}"] {
        assert_eq!(
            host.change_password(CLERK, "caf\u{e9}", same),
            Err(PasswordChangeError::Unchanged)
        );
    }
    assert_eq!(login(&host, &client, CLERK, "cafe\u{301}"), 0);
    assert!(host.close());
}

/// Change Admin's empty password to `OM_NEW_PASSWORD` in the copy of a
/// FileMaker-made file at `OM_CHANGE_PASSWORD_FILE` (a starter solution, so
/// Admin has no password), for FileMaker Pro to open afterwards: the check
/// that it reads the account OpenMaker wrote (a wrong checksum is 808).
#[test]
#[ignore = "requires OM_CHANGE_PASSWORD_FILE (a writable copy of a starter solution) and OM_NEW_PASSWORD"]
fn admin_password_changed_for_filemaker_pro() {
    let path = PathBuf::from(std::env::var_os("OM_CHANGE_PASSWORD_FILE").expect("a file"));
    let new = std::env::var("OM_NEW_PASSWORD").expect("a password");
    let client = client();
    let host = LocalHost::open(&path, &client.cert).unwrap();
    host.change_password("Admin", "", &new).unwrap();
    assert!(host.close());
}

/// Each `account<TAB>password` line of the file at `OM_LOGINS` logs in to
/// a copy of the FileMaker-made file at `OM_LOGIN_FILE`, and its password
/// with one more character is refused (212). The check of FileMaker Pro's
/// accounts with passwords outside ASCII (#211), whose file and passwords
/// stay out of the repository.
#[test]
#[ignore = "requires OM_LOGIN_FILE (a FileMaker-made file) and OM_LOGINS (account<TAB>password lines)"]
fn filemaker_made_accounts_log_in() {
    let source = PathBuf::from(std::env::var_os("OM_LOGIN_FILE").expect("a file"));
    let logins = zeroize::Zeroizing::new(
        std::fs::read_to_string(std::env::var_os("OM_LOGINS").expect("a list of logins")).unwrap(),
    );
    let folder = Folder::new("made-logins");
    let path = folder.0.join("made.fmp12");
    std::fs::copy(&source, &path).unwrap();
    let client = client();
    let host = LocalHost::open(&path, &client.cert).unwrap();
    // `login` checks the synthetic file's creation stamp; this file has its
    // own.
    let login = |account: &str, password: &str| {
        let mut session = connect(&host, &client).unwrap();
        session.open_file("127.0.0.1", host.file_name()).unwrap();
        let creation = session.fetch_internal_creation().unwrap();
        let environment = LoginEnvironment::for_remote_file("127.0.0.1", host.file_name());
        let reply = session
            .login(account, password, creation, &environment)
            .unwrap();
        session.disconnect().unwrap();
        reply.error_code
    };
    let mut accounts = 0;
    for line in logins.lines().filter(|line| !line.is_empty()) {
        let (account, password) = line.split_once('\t').expect("account<TAB>password");
        assert_eq!(login(account, password), 0, "{account}");
        let wrong = zeroize::Zeroizing::new(format!("{password}x"));
        assert_eq!(login(account, &wrong), 212, "{account}");
        accounts += 1;
    }
    assert!(accounts > 0);
    assert!(host.close());
}

#[test]
fn guest_external_expired_and_unreadable_accounts_are_refused() {
    let folder = Folder::new("forms");
    let client = client();
    let cases: [(&str, Change, &str, &str, u16); 6] = [
        // [Guest] (record 01), active, in [Full Access], with a name and
        // password: still no typed password opens it.
        (
            "guest",
            |root| {
                let password = om_crypto::account::StoredPassword::encode("visit", [1; 10]);
                put_member(root, &[23, 1, 5, 1, 6], password);
                put_member(root, &[23, 1, 5, 1, 0x0a], fixture::ACTIVE_STATUS.to_vec());
                put_member(root, &[23, 1, 5, 1, 0x0b], vec![1, 1]);
                root.put(
                    &[&[23], &[1], &[5], &[1], &[16]],
                    om_scripts::text_node(&[], "Visitor"),
                );
            },
            "Visitor",
            "visit",
            212,
        ),
        // An account another service authenticates (kind 2): no typed
        // password either.
        (
            "external",
            |root| put_member(root, &[23, 1, 5, 2, 2], vec![2]),
            ACCOUNT,
            PASSWORD,
            212,
        ),
        // [Full Access] passwords last 30 days, and Admin's was set long ago.
        (
            "expired",
            |root| put_member(root, &[23, 2, 5, 1, 0x0c], vec![0, 30]),
            ACCOUNT,
            PASSWORD,
            LOGIN_PASSWORD_CHANGE_DUE,
        ),
        // A status, a kind or a password lifetime in another form.
        (
            "status",
            |root| set_status(root, 2, Some(&[0x06, 0x90, 0x00, 0x41, 0x80, 0x00][..])),
            ACCOUNT,
            PASSWORD,
            LOGIN_UNREADABLE_ACCOUNT,
        ),
        (
            "kind",
            |root| put_member(root, &[23, 1, 5, 2, 2], vec![0, 0]),
            ACCOUNT,
            PASSWORD,
            LOGIN_UNREADABLE_ACCOUNT,
        ),
        (
            "lifetime",
            |root| put_member(root, &[23, 2, 5, 1, 0x0c], vec![30]),
            ACCOUNT,
            PASSWORD,
            LOGIN_UNREADABLE_ACCOUNT,
        ),
    ];
    for (tag, change, account, password, code) in cases {
        let path = folder.tasks_with(&format!("{tag}.fmp12"), change);
        let before = std::fs::read(&path).unwrap();
        let host = LocalHost::open(&path, &client.cert).unwrap();
        assert_eq!(login(&host, &client, account, password), code, "{tag}");
        assert!(host.close());
        assert_eq!(std::fs::read(&path).unwrap(), before, "{tag}");
    }
}

/// Set the leaf at the one-byte segments `path` to `value`.
fn put_member(root: &mut OwnedNode, path: &[u8], value: Vec<u8>) {
    let segments: Vec<[u8; 1]> = path.iter().map(|&s| [s]).collect();
    let segments: Vec<&[u8]> = segments.iter().map(|s| &s[..]).collect();
    root.put(&segments, OwnedNode::leaf(Vec::new(), value));
}

/// A change to the fixture's tree.
type Change = fn(&mut OwnedNode);

/// Sets `01`-`03` whose members are not the built-in sets' (their names
/// are not checked, #876), and a custom set named like one, are refused at
/// login (9), not read as custom sets (local-files.md 2.3, #729).
#[test]
fn a_built_in_set_that_is_not_the_verified_one_is_refused() {
    let folder = Folder::new("sets");
    let client = client();
    // Set `01` with the other privileges `0a` every FileMaker file gives
    // it, so that it would decode as a custom set (#729).
    fn full_access_options(root: &mut OwnedNode) {
        root.put(
            &[&[23], &[2], &[5], &[1], &[0x0a]],
            OwnedNode::leaf(Vec::new(), vec![0x9f, 0xff]),
        );
    }
    let changes: [(&str, Change, &str, &str); 3] = [
        // [Full Access] with its record privileges limited.
        (
            "limited",
            |root| {
                full_access_options(root);
                root.put(
                    &[&[23], &[2], &[5], &[1], &[2]],
                    OwnedNode::leaf(Vec::new(), vec![0x10, 0x03]),
                )
            },
            ACCOUNT,
            PASSWORD,
        ),
        // [Data Entry Only] with printing off: Clerk is refused too.
        (
            "data-entry-changed",
            |root| {
                fixture::put_data_entry_only(root);
                root.put(
                    &[&[23], &[2], &[5], &[2], &[0x0a]],
                    OwnedNode::leaf(Vec::new(), vec![0x02, 0x04]),
                )
            },
            CLERK,
            CLERK_PASSWORD,
        ),
        // Admin in a custom set that calls itself [Full Access].
        (
            "custom",
            |root| {
                root.put(
                    &[&[23], &[2], &[5], &[4], &[16]],
                    om_scripts::text_node(&[], "[Full Access]"),
                );
                root.put(
                    &[&[23], &[2], &[5], &[4], &[2]],
                    OwnedNode::leaf(Vec::new(), vec![0x55, 0x03]),
                );
                root.put(
                    &[&[23], &[1], &[5], &[2], &[0x0b]],
                    OwnedNode::leaf(Vec::new(), vec![1, 4]),
                );
            },
            ACCOUNT,
            PASSWORD,
        ),
    ];
    for (tag, change, account, password) in changes {
        let path = folder.tasks_with(&format!("{tag}.fmp12"), change);
        let host = LocalHost::open(&path, &client.cert).unwrap();
        assert_eq!(
            login(&host, &client, account, password),
            LOGIN_UNSUPPORTED_PRIVILEGES,
            "{tag}"
        );
    }
}

#[test]
fn an_account_without_a_stored_password_cannot_log_in() {
    // The Tasks file without its stored passwords: nothing to check a
    // password against, so no password is accepted, not even an empty one.
    let folder = Folder::new("no-passwords");
    let path = folder.0.join(FILE);
    let mut root = fixture::tree(&[], true);
    for record in [2u8, 3] {
        root.remove(&[&[23], &[1], &[5], &[record], &[6]]).unwrap();
    }
    fixture::write_tree(&path, &root);
    let client = client();
    let host = LocalHost::open(&path, &client.cert).unwrap();
    assert_eq!(host.suggested_account(), None);
    assert_eq!(login(&host, &client, ACCOUNT, ""), 212);
    assert_eq!(login(&host, &client, ACCOUNT, PASSWORD), 212);
}

#[test]
fn deferred_work_recovered_on_opening_waits_for_an_admitted_login() {
    let folder = Folder::new("recovery");
    // A table whose deferred field work list (`[T, 03, 01, fd]`) a stop
    // left in the file: opening finishes it (removing the list).
    let path = folder.tasks_with(FILE, |root| {
        root.put(
            &[&uint_segment(TABLE), &[3], &[1], &[0xfd]],
            OwnedNode::leaf(Vec::new(), Vec::new()),
        );
    });
    let before = std::fs::read(&path).unwrap();
    let client = client();
    for (account, password) in [(ACCOUNT, "wrong"), (CLERK, CLERK_PASSWORD)] {
        let host = LocalHost::open(&path, &client.cert).unwrap();
        assert_ne!(login(&host, &client, account, password), 0);
        assert!(host.close());
        assert_eq!(std::fs::read(&path).unwrap(), before, "{account}");
    }
    // Opened and closed without any login: nothing either.
    assert!(LocalHost::open(&path, &client.cert).unwrap().close());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let host = LocalHost::open(&path, &client.cert).unwrap();
    assert_eq!(login(&host, &client, ACCOUNT, PASSWORD), 0);
    assert!(host.close());
    let after = om_hbam::HbamFile::open(&path).unwrap();
    assert!(after
        .subtree(&[&uint_segment(TABLE), &[3], &[1], &[0xfd]])
        .unwrap()
        .is_none());
    assert_eq!(folder.files(), [FILE]);
}

#[test]
fn only_the_opening_clients_certificate_is_accepted() {
    let folder = Folder::new("pinned");
    let path = folder.tasks(FILE);
    let owner = client();
    let host = LocalHost::open(&path, &owner.cert).unwrap();
    let other = client();
    assert!(connect(&host, &other).is_err());
    assert_eq!(login(&host, &owner, ACCOUNT, PASSWORD), 0);
}

#[test]
fn a_file_opens_once_through_any_of_its_paths_until_it_is_closed() {
    let folder = Folder::new("once");
    let path = folder.tasks(FILE);
    let client = client();
    let host = LocalHost::open(&path, &client.cert).unwrap();
    let port = host.port();
    let again = LocalHost::open(&path, &client.cert).unwrap_err();
    assert!(matches!(again, LocalHostError::InUse(_)), "{again:?}");
    assert_eq!(again.code(), 300);
    #[cfg(unix)]
    {
        let alias = folder.0.join("alias.fmp12");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        let through_alias = LocalHost::open(&alias, &client.cert).unwrap_err();
        assert!(
            matches!(&through_alias, LocalHostError::InUse(p) if p == &host.path().to_path_buf()),
            "{through_alias:?}"
        );
    }
    assert!(host.close());
    // Closed: the port is free and the file opens again.
    assert!(std::net::TcpStream::connect(("127.0.0.1", port)).is_err());
    let reopened = LocalHost::open(&path, &client.cert).unwrap();
    assert_eq!(login(&reopened, &client, ACCOUNT, PASSWORD), 0);
}

/// Save a Copy as: a copy is the file's bytes, a compacted copy its tree
/// rewritten, both open as files of their own; nothing that is already
/// there is replaced, and clones are refused.
#[cfg(target_os = "linux")]
#[test]
fn saved_copies_are_new_files_that_open_and_replace_nothing() {
    use om_data::CopyKind;
    use std::os::unix::fs::PermissionsExt;
    let folder = Folder::new("save-copy");
    std::fs::set_permissions(&folder.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = folder.tasks(FILE);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
    let original = std::fs::read(&path).unwrap();
    let client = client();
    let host = LocalHost::open(&path, &client.cert).unwrap();

    let copy = folder.0.join("Copy.fmp12");
    match host.prepare_copy(&copy, CopyKind::Copy) {
        Ok(publish) => publish().unwrap(),
        Err(err)
            if err.kind() == std::io::ErrorKind::Unsupported
                && err
                    .to_string()
                    .starts_with("copy destinations require ext2") =>
        {
            // Overlay-root test runners exercise refusal, even when their
            // temporary directory is on a supported mount.
            assert!(!copy.exists());
            assert_eq!(std::fs::read(&path).unwrap(), original);
            assert!(host.close());
            return;
        }
        Err(err) => panic!("prepare copy: {err}"),
    }
    assert_eq!(std::fs::read(&copy).unwrap(), original);
    let compacted = folder.0.join("Compacted.fmp12");
    host.save_copy(&compacted, CopyKind::Compacted).unwrap();
    let tree = |path: &Path| {
        let file = om_hbam::HbamFile::open(path).unwrap();
        let tree = file.key_tree().unwrap();
        tree.root.to_owned_node()
    };
    assert_eq!(tree(&compacted), tree(&path));
    for made in [&copy, &compacted] {
        let mode = std::fs::metadata(made).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{mode:o}");
    }

    // What is there stays: another file, the open file itself, and names
    // of the kind its saves make their new copy under.
    let taken = folder.0.join("Taken.fmp12");
    let pending = host.prepare_copy(&taken, CopyKind::Copy).unwrap();
    assert!(!taken.exists());
    std::fs::write(&taken, b"someone else's").unwrap();
    let refused = pending().unwrap_err();
    assert_eq!(
        refused.kind(),
        std::io::ErrorKind::AlreadyExists,
        "{refused}"
    );
    assert_eq!(std::fs::read(&taken).unwrap(), b"someone else's");
    let refused = host.save_copy(&path, CopyKind::Compacted).unwrap_err();
    assert_eq!(
        refused.kind(),
        std::io::ErrorKind::AlreadyExists,
        "{refused}"
    );
    let reserved = folder.0.join(format!("{FILE}.omsave"));
    let refused = host.save_copy(&reserved, CopyKind::Copy).unwrap_err();
    assert_eq!(refused.kind(), std::io::ErrorKind::Unsupported, "{refused}");
    assert!(!reserved.exists());

    let blocked = folder.0.join("Blocked.fmp12");
    std::fs::set_permissions(&folder.0, std::fs::Permissions::from_mode(0o770)).unwrap();
    let refused = host.save_copy(&blocked, CopyKind::Copy).unwrap_err();
    std::fs::set_permissions(&folder.0, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(refused.kind(), std::io::ErrorKind::Unsupported, "{refused}");
    assert!(!blocked.exists());

    let clone = folder.0.join("Clone.fmp12");
    let refused = host.save_copy(&clone, CopyKind::Clone).unwrap_err();
    assert_eq!(refused.kind(), std::io::ErrorKind::Unsupported, "{refused}");
    assert_eq!(std::fs::read(&path).unwrap(), original);

    // After a save (the password changed), a copy is the file as saved.
    const CHANGED: &str = "another horse";
    host.change_password(ACCOUNT, PASSWORD, CHANGED).unwrap();
    let saved = folder.0.join("Saved.fmp12");
    host.save_copy(&saved, CopyKind::Copy).unwrap();
    assert_eq!(
        std::fs::read(&saved).unwrap(),
        std::fs::read(&path).unwrap()
    );
    // Nothing else is left beside them: no staging entries.
    assert_eq!(
        folder.files(),
        [
            "Compacted.fmp12",
            "Copy.fmp12",
            "Saved.fmp12",
            "Taken.fmp12",
            FILE
        ]
    );
    assert!(host.close());

    // Each copy is a file of its own, with the file's accounts.
    for (made, password) in [(&copy, PASSWORD), (&compacted, PASSWORD), (&saved, CHANGED)] {
        let opened = LocalHost::open(made, &client.cert).unwrap();
        assert_eq!(opened.read_only(), None);
        assert_eq!(login(&opened, &client, ACCOUNT, password), 0);
        assert!(opened.close());
    }
}

#[test]
fn a_connection_stalled_before_its_handshake_does_not_keep_the_file() {
    let folder = Folder::new("stalled");
    let path = folder.tasks(FILE);
    let client = client();
    let host = LocalHost::open(&path, &client.cert).unwrap();
    // Accepted, then silent: no TLS handshake, no session.
    let stalled = std::net::TcpStream::connect(("127.0.0.1", host.port())).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let started = Instant::now();
    assert!(host.close(), "the host still holds the file");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    // Released: the file opens (and locks) again at once.
    let reopened = LocalHost::open(&path, &client.cert).unwrap();
    assert_eq!(login(&reopened, &client, ACCOUNT, PASSWORD), 0);
    drop(stalled);
}

#[test]
fn files_it_cannot_read_are_refused_and_left_as_they_were() {
    let folder = Folder::new("refused");
    let client = client();
    let tasks = std::fs::read(folder.tasks("source.fmp12")).unwrap();
    let cases: [(&str, Vec<u8>, i32); 4] = [
        ("text.fmp12", b"not a FileMaker file".to_vec(), 807),
        ("empty.fmp12", Vec::new(), 807),
        // Half a file: whole pages, with the tree cut off.
        (
            "half.fmp12",
            tasks[..tasks.len() / 2 / 4096 * 4096].to_vec(),
            805,
        ),
        // A page torn in two.
        ("torn.fmp12", tasks[..tasks.len() - 100].to_vec(), 805),
    ];
    for (name, bytes, code) in cases {
        let path = folder.0.join(name);
        std::fs::write(&path, &bytes).unwrap();
        let error = LocalHost::open(&path, &client.cert).unwrap_err();
        assert_eq!(error.code(), code, "{name}: {error}");
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "{name} changed");
    }
    let missing = LocalHost::open(&folder.0.join("missing.fmp12"), &client.cert).unwrap_err();
    assert!(matches!(missing, LocalHostError::Missing(_)), "{missing:?}");
    let folder_path = LocalHost::open(&folder.0, &client.cert).unwrap_err();
    assert!(
        matches!(folder_path, LocalHostError::NotAFile(_)),
        "{folder_path:?}"
    );
    // Nothing was written beside them either.
    assert_eq!(
        folder.files(),
        [
            "empty.fmp12",
            "half.fmp12",
            "source.fmp12",
            "text.fmp12",
            "torn.fmp12"
        ]
    );
}

/// Whether this process can write a file it made read-only (as root can),
/// so read-only checks would be meaningless.
#[cfg(unix)]
fn writes_read_only_files(path: &Path) -> bool {
    std::fs::OpenOptions::new().write(true).open(path).is_ok()
}

#[cfg(unix)]
#[test]
fn a_file_it_may_not_write_is_served_read_only_and_refuses_write_requests() {
    use std::os::unix::fs::PermissionsExt;
    let folder = Folder::new("read-only");
    let path = folder.tasks(FILE);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    if writes_read_only_files(&path) {
        eprintln!("skipped: this user writes read-only files");
        return;
    }
    let before = std::fs::read(&path).unwrap();
    let client = client();
    let host = LocalHost::open(&path, &client.cert).unwrap();
    assert_eq!(host.read_only(), Some(ReadOnly::NotWritable));
    // Read-only files can be read by others at the same time.
    let reader = LocalHost::open(&path, &client.cert).unwrap();
    assert!(reader.is_read_only());
    let mut session = logged_in(&host, &client, ACCOUNT, PASSWORD).unwrap();
    // The write requests themselves are refused, before they change
    // anything (200, access denied).
    let created = session.create_record(&uint_segment(TABLE), 7).unwrap();
    assert_eq!(created.status, 200, "{created:?}");
    session.disconnect().unwrap();
    assert!(host.close());
    assert!(reader.close());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(folder.files(), [FILE]);
}

#[cfg(unix)]
#[test]
fn files_a_save_could_not_replace_safely_are_served_read_only() {
    let folder = Folder::new("unsafe-saves");
    let client = client();
    // Another name for the file: a save's new copy would leave it behind.
    let linked = folder.tasks("linked.fmp12");
    std::fs::hard_link(&linked, folder.0.join("other name.fmp12")).unwrap();
    let host = LocalHost::open(&linked, &client.cert).unwrap();
    assert_eq!(host.read_only(), Some(ReadOnly::OtherNames));
    assert!(host.close());
    // An entry where the save's new copy goes, here a link to another file.
    let crowded = folder.tasks("crowded.fmp12");
    let victim = folder.0.join("victim");
    std::fs::write(&victim, b"not to be truncated").unwrap();
    std::os::unix::fs::symlink(&victim, folder.0.join("crowded.fmp12.omsave")).unwrap();
    let before = std::fs::read(&crowded).unwrap();
    let host = LocalHost::open(&crowded, &client.cert).unwrap();
    assert_eq!(host.read_only(), Some(ReadOnly::CopyInTheWay));
    let mut session = logged_in(&host, &client, ACCOUNT, PASSWORD).unwrap();
    assert_eq!(
        session
            .create_record(&uint_segment(TABLE), 7)
            .unwrap()
            .status,
        200
    );
    session.disconnect().unwrap();
    assert!(host.close());
    assert_eq!(std::fs::read(&crowded).unwrap(), before);
    assert_eq!(std::fs::read(&victim).unwrap(), b"not to be truncated");
    assert!(
        std::fs::symlink_metadata(folder.0.join("crowded.fmp12.omsave"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

/// A copy a save left beside a local file when it stopped part way is
/// removed once the open holds the file's lock for writing, and the file
/// opens writable and saves; an entry there that a save never leaves, a
/// file with another name, is left and the file opens read-only
/// (docs/server-storage.md 5).
#[cfg(unix)]
#[test]
fn a_copy_a_save_left_is_removed_when_a_file_opens() {
    const CHANGED: &str = "after a crash";
    let folder = Folder::new("leftover");
    let path = folder.tasks(FILE);
    let copy = folder.0.join(format!("{FILE}.omsave"));
    std::fs::write(&copy, b"half a save").unwrap();
    let client = client();
    let host = LocalHost::open(&path, &client.cert).unwrap();
    assert_eq!(host.read_only(), None);
    assert!(!copy.exists(), "removed");
    host.change_password(ACCOUNT, PASSWORD, CHANGED).unwrap();
    assert!(host.close());
    let again = LocalHost::open(&path, &client.cert).unwrap();
    assert_eq!(login(&again, &client, ACCOUNT, CHANGED), 0, "saved");
    assert!(again.close());
    assert_eq!(folder.files(), [FILE]);

    // A file with another name is not a copy a save left.
    let other = folder.0.join("other name");
    std::fs::write(&other, b"someone's file").unwrap();
    std::fs::hard_link(&other, &copy).unwrap();
    let host = LocalHost::open(&path, &client.cert).unwrap();
    assert_eq!(host.read_only(), Some(ReadOnly::CopyInTheWay));
    assert!(host.close());
    assert_eq!(std::fs::read(&copy).unwrap(), b"someone's file");
}

#[test]
fn read_only_queries_refuse_schema_writes_before_execution() {
    use om_query::program::{leaf, opcode};
    let folder = Folder::new("readonly-schema-query");
    let path = folder.tasks(FILE);
    let before_bytes = std::fs::read(&path).unwrap();
    // A synthetic collision makes this read-only on every supported Unix
    // host: a folder where a save makes its copy, which an open never
    // removes (a regular file there would be a copy a save left, removed).
    let copy = folder.0.join(format!("{FILE}.omsave"));
    std::fs::create_dir(&copy).unwrap();
    let client = client();
    let host = LocalHost::open(&path, &client.cert).unwrap();
    assert_eq!(host.read_only(), Some(ReadOnly::CopyInTheWay));
    let mut session = logged_in(&host, &client, ACCOUNT, PASSWORD).unwrap();
    let table = uint_segment(TABLE);
    let before = session.download(&[&table]).unwrap().node_state;
    let file_id = session.opened_file().unwrap().reply.file_id;
    let mut program = OwnedNode::list(Vec::new(), Vec::new());
    for (key, value) in [
        (leaf::OPCODE, vec![opcode::DELETE_TABLE]),
        (leaf::SRC, [vec![table.len() as u8], table.clone()].concat()),
        (leaf::FILE_ID, file_id.to_be_bytes().to_vec()),
        (
            leaf::ACCESS_KEY,
            om_crypto::query_access_key(1, session.connection_key(), file_id).to_vec(),
        ),
    ] {
        program.put(&[&[1], &[1], &[key]], OwnedNode::leaf(Vec::new(), value));
    }
    for operation in [
        opcode::DELETE_TABLE,
        opcode::UPLOAD_MULTIPLE,
        opcode::GENERATE_MEMBER_KEY,
    ] {
        program.put(
            &[&[1], &[1], &[leaf::OPCODE]],
            OwnedNode::leaf(Vec::new(), vec![operation]),
        );
        let reply = session.query(&program.to_wire().unwrap()).unwrap();
        assert!(
            reply.slots.windows(3).any(|s| s == [2, 0, 200]),
            "{operation}: {:02x?}",
            reply.slots
        );
        assert_eq!(session.download(&[&table]).unwrap().node_state, before);
    }
    // Read-only queries still execute, including catalog reads.
    program.put(
        &[&[1], &[1], &[leaf::OPCODE]],
        OwnedNode::leaf(Vec::new(), vec![opcode::GET_KEY]),
    );
    let reply = session.query(&program.to_wire().unwrap()).unwrap();
    assert!(!reply.slots.windows(3).any(|s| s == [2, 0, 200]));
    let after = session.download(&[&table]).unwrap().node_state;
    session.disconnect().unwrap();
    assert!(host.close());
    assert_eq!(std::fs::read(&path).unwrap(), before_bytes);
    assert!(copy.is_dir(), "the collision is left as it was");
    assert_eq!(
        after, before,
        "read-only op86 DeleteTable changed the live store"
    );
}

#[test]
fn one_day_password_lifetime_accepts_its_set_date() {
    let today = jiff::Zoned::now().date();
    let status = om_server::accounts::AccountStatus {
        date: today,
        ..om_server::accounts::AccountStatus::ABSENT
    };
    let value = 0x800000
        | (u32::from(today.year() as u16) << 10)
        | (u32::from(today.month() as u8) << 6)
        | u32::from(today.day() as u8);
    let stamp = vec![
        5,
        (value >> 16) as u8,
        (value >> 8) as u8,
        value as u8,
        0x80,
        0,
    ];
    assert_eq!(
        om_server::accounts::AccountStatus::decode(Some(&stamp)),
        Some(status)
    );
    let folder = Folder::new("one-day-lifetime");
    let path = folder.tasks_with("one-day.fmp12", |root| {
        put_member(root, &[23, 1, 5, 2, 0x0a], stamp);
        put_member(root, &[23, 2, 5, 1, 0x0c], vec![0, 1]);
    });
    let before = std::fs::read(&path).unwrap();
    let client = client();
    let host = LocalHost::open(&path, &client.cert).unwrap();
    let code = login(&host, &client, ACCOUNT, PASSWORD);
    assert_eq!(login(&host, &client, ACCOUNT, "wrong"), 212);
    assert!(host.close());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(
        code, 0,
        "native TestFMPassword expires at elapsed >= lifetime; 0 days < 1"
    );
}

/// A hosted file's logins are checked against its own accounts (#154): the
/// password the file stores, then the account's state and set, as for a
/// local file, except that a server refuses a custom set (#734); and what
/// each logged-in account may read and is told.
#[test]
fn hosted_file_accounts_obey_state_and_notification_boundaries() {
    use om_protocol::{write, Opcode};
    use om_server::{HostedFile, Identity, Server, ServerConfig, ServerIdentity};

    let folder = Folder::new("remote-account-state");
    let path = folder.tasks_with(FILE, |root| {
        // Clerk in the whole [Data Entry Only]; Unenforced in a set without
        // a name, which the host cannot hold it to; Office in a custom set,
        // which a server does not hold a client to every member of yet.
        fixture::put_data_entry_only(root);
        // Both keys must exist so the whole-member filtering assertions can
        // catch a widened allow-list, without changing the shared fixture.
        root.put(
            &[&[23], &[1], &[5], &[3], &[4]],
            OwnedNode::leaf(Vec::new(), vec![0xa4; 16]),
        );
        root.put(
            &[&[23], &[1], &[5], &[3], &[0x5d]],
            OwnedNode::leaf(Vec::new(), vec![0x5d]),
        );
        for (record, name, password, set) in [
            (7u8, "Unenforced", "unenforced", 6u8),
            (8, "Office", "office", 7),
        ] {
            fixture::put_account(root, record, password, None, set);
            root.put(
                &[&[23], &[1], &[5], &[record], &[16]],
                om_scripts::text_node(&[], name),
            );
            root.put(
                &[&[23], &[2], &[5], &[set], &[0x0c]],
                OwnedNode::leaf(Vec::new(), vec![0, 0]),
            );
        }
        root.put(
            &[&[23], &[2], &[5], &[7], &[16]],
            om_scripts::text_node(&[], "Office"),
        );
        for (member, value) in [(2u8, [0x55u8, 0x03]), (0x0a, [0x03, 0x04])] {
            root.put(
                &[&[23], &[2], &[5], &[7], &[member]],
                OwnedNode::leaf(Vec::new(), value.to_vec()),
            );
        }
        for (catalog, quad) in [(0x04u8, 0xdeu8), (0x11, 0xd6), (0x21, 0xd6)] {
            root.put(
                &[&[23], &[2], &[5], &[7], &[9], &[catalog]],
                OwnedNode::leaf(Vec::new(), vec![quad]),
            );
        }
        // Expired: its set's passwords last 30 days, and its password was set on
        // 1/1/1024.
        fixture::put_account(root, 6, "expired", Some(&fixture::ACTIVE_STATUS[..]), 3);
        root.put(
            &[&[23], &[1], &[5], &[6], &[16]],
            om_scripts::text_node(&[], "Expired"),
        );
        root.put(
            &[&[23], &[2], &[5], &[3], &[0x0c]],
            OwnedNode::leaf(Vec::new(), vec![0, 30]),
        );
    });
    let config = ServerConfig {
        identity: ServerIdentity {
            name: "synthetic".into(),
            version_string: "test".into(),
            server_id: "0123456789abcdef0123456789abcdef".into(),
            web_service_settings: String::new(),
        },
        files: vec![HostedFile {
            name: FILE.into(),
            path,
        }],
        insecure_files: false,
        record_dir: None,
    };
    let server = Server::bind(
        "127.0.0.1:0",
        config,
        Identity::self_signed(vec!["localhost".into()]).unwrap(),
    )
    .unwrap();
    let port = server.local_addr().unwrap().port();
    let handle = server.handle().unwrap();
    let thread = std::thread::spawn(move || server.serve());
    let open = || {
        let c = client();
        let mut s = Session::connect(
            "127.0.0.1",
            port,
            &c.cert,
            &c.key,
            "127.0.0.1",
            &MachineInfo::default(),
        )
        .unwrap();
        let file = s.open_file("127.0.0.1", FILE).unwrap().file_id;
        assert_eq!(s.fetch_internal_creation().unwrap(), INTERNAL_CREATION);
        (s, file)
    };
    let reply = |s: &mut Session, name, password| {
        s.login(
            name,
            password,
            INTERNAL_CREATION,
            &LoginEnvironment::for_remote_file("127.0.0.1", FILE),
        )
        .unwrap()
    };
    let login = |s: &mut Session, name, password| reply(s, name, password).error_code;
    for (name, password, error) in [
        // Another account's password, and a name that is no account's.
        (ACCOUNT, CLERK_PASSWORD, 212),
        ("Nobody", PASSWORD, 212),
        (FORMER, FORMER_PASSWORD, LOGIN_INACTIVE_ACCOUNT),
        (NEWCOMER, NEWCOMER_PASSWORD, LOGIN_PASSWORD_CHANGE_DUE),
        ("Expired", "expired", LOGIN_PASSWORD_CHANGE_DUE),
        ("Unenforced", "unenforced", LOGIN_UNSUPPORTED_PRIVILEGES),
        ("Office", "office", LOGIN_UNSUPPORTED_PRIVILEGES),
    ] {
        let (mut s, _) = open();
        let refused = reply(&mut s, name, password);
        assert_eq!(refused.error_code, error, "{name}");
        // As FileMaker Server refuses one: version 9, an empty key, the
        // error (network-protocol.md 5.6.5).
        assert_eq!((refused.version, refused.result_key.len()), (Some(9), 0));
        s.disconnect().unwrap();
    }
    let (mut prelogin, pre_file) = open();
    assert!(prelogin.request_notifications().is_err());
    let (mut admin, admin_file) = open();
    assert_eq!(login(&mut admin, ACCOUNT, PASSWORD), 0);
    admin.request_notifications().unwrap();
    let (mut clerk, clerk_file) = open();
    assert_eq!(login(&mut clerk, CLERK, CLERK_PASSWORD), 0);
    clerk.request_notifications().unwrap();

    // FileMaker Pro fetches the account member named by Login before it
    // draws a hosted layout. Give it the signed-in account's metadata,
    // while neither its password verifier nor another account's member
    // becomes readable to [Data Entry Only].
    let own = clerk.download(&[&[23], &[1], &[5], &[3]]).unwrap();
    assert_eq!(own.lock_error, 0);
    let own = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::new(&own.slots)).unwrap();
    assert!(own.root.child(&[0x0b]).is_some());
    assert!(own.root.child(&[4]).is_none());
    assert!(own.root.child(&[6]).is_none());
    assert!(own.root.child(&[0x5d]).is_none());
    for child in [4u8, 6] {
        assert_eq!(
            clerk
                .download(&[&[23], &[1], &[5], &[3], &[child]])
                .unwrap()
                .lock_error,
            200
        );
    }
    let unlocked = om_protocol::build_download_from_host(&om_protocol::DownloadRequest {
        transaction_id: 76,
        file_id: clerk_file,
        key_path: &[&[23], &[1], &[5], &[3]],
        resume: om_protocol::Resume::start(),
    });
    let unlocked =
        om_protocol::parse_download_from_host_reply(&clerk.perform(&unlocked).unwrap()).unwrap();
    let unlocked = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::new(&unlocked.slots)).unwrap();
    assert!(unlocked.root.child(&[0x0b]).is_some());
    assert!(unlocked.root.child(&[4]).is_none());
    assert!(unlocked.root.child(&[6]).is_none());
    assert!(unlocked.root.child(&[0x5d]).is_none());
    let selected = clerk
        .download_list(
            &[&[23], &[1], &[5], &[3]],
            &[&[4], &[6], &[0x0b], &[0x5d]],
            0,
        )
        .unwrap();
    assert_eq!(selected.status, 0);
    let selected = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::new(&selected.slots)).unwrap();
    assert!(selected.root.child(&[0x0b]).is_some());
    assert!(selected.root.child(&[4]).is_none());
    assert!(selected.root.child(&[6]).is_none());
    assert!(selected.root.child(&[0x5d]).is_none());
    assert_eq!(
        clerk
            .download(&[&[23], &[1], &[5], &[2]])
            .unwrap()
            .lock_error,
        200
    );

    let password_path: &[&[u8]] = &[&[23], &[1], &[5], &[2], &[6]];
    assert_eq!(clerk.download(password_path).unwrap().lock_error, 200);
    assert_eq!(
        clerk
            .download(&[&[23], &[1], &[5], &[2], &[4]])
            .unwrap()
            .lock_error,
        200
    );
    let unlocked = om_protocol::build_download_from_host(&om_protocol::DownloadRequest {
        transaction_id: 77,
        file_id: clerk_file,
        key_path: password_path,
        resume: om_protocol::Resume::start(),
    });
    let reply =
        om_protocol::parse_download_from_host_reply(&clerk.perform(&unlocked).unwrap()).unwrap();
    assert!(reply.value.is_empty() && reply.slots.is_empty());
    assert_eq!(
        clerk.download(&[&[23], &[1], &[5]]).unwrap().lock_error,
        200
    );
    assert_eq!(
        clerk
            .download_list(&[&[23], &[1]], &[&[5]], 0)
            .unwrap()
            .status,
        200
    );
    assert_eq!(
        clerk
            .download(&[&[23], &[1], &[5], &[2], &[16]])
            .unwrap()
            .lock_error,
        0
    );
    assert_eq!(
        clerk.download(&[&uint_segment(TABLE)]).unwrap().lock_error,
        0
    );
    let secret = admin.download(password_path).unwrap().value;
    assert!(!secret.is_empty());
    let mut query = OwnedNode::list(Vec::new(), Vec::new());
    use om_query::program::{leaf, opcode};
    for (key, value) in [
        (leaf::OPCODE, vec![opcode::GET_KEY]),
        (
            leaf::SRC,
            password_path
                .iter()
                .flat_map(|segment| {
                    std::iter::once(segment.len() as u8).chain(segment.iter().copied())
                })
                .collect(),
        ),
        (leaf::DEST, vec![1, 3, 1, 1]),
        (leaf::FILE_ID, admin_file.to_be_bytes().to_vec()),
    ] {
        query.put(&[&[1], &[1], &[key]], OwnedNode::leaf(Vec::new(), value));
    }
    let program = query.to_wire().unwrap();
    assert!(admin
        .query(&program)
        .unwrap()
        .slots
        .windows(secret.len())
        .any(|part| part == secret));
    assert!(!clerk
        .query(&program)
        .unwrap()
        .slots
        .windows(secret.len())
        .any(|part| part == secret));
    query.put(
        &[&[1], &[1], &[leaf::OPCODE]],
        OwnedNode::leaf(Vec::new(), vec![opcode::NODE_EXISTS]),
    );
    let exists = |s: &mut Session, path: &[&[u8]], query: &mut OwnedNode| {
        let encoded: Vec<u8> = path
            .iter()
            .flat_map(|segment| std::iter::once(segment.len() as u8).chain(segment.iter().copied()))
            .collect();
        query.put(
            &[&[1], &[1], &[leaf::SRC]],
            OwnedNode::leaf(Vec::new(), encoded),
        );
        let slots = s.query(&query.to_wire().unwrap()).unwrap().slots;
        let tree = om_hbam::KeyTree::from_slots(om_hbam::SlotReader::new(&slots)).unwrap();
        tree.node(&[&[1], &[0]]).and_then(|node| node.value) == Some(&[1, 1][..])
    };
    let own = exists(&mut clerk, &[&[23], &[1], &[5], &[3]], &mut query);
    let other = exists(&mut clerk, &[&[23], &[1], &[5], &[2]], &mut query);
    assert!(own);
    assert!(!other);
    assert!(!exists(
        &mut clerk,
        &[&[23], &[1], &[5], &[3], &[6]],
        &mut query
    ));

    let table = uint_segment(TABLE);
    let lock_path: &[&[u8]] = &[&table, &[5], &[1]];
    let unlock = |s: &mut Session, file: u32, txn: u32| {
        let req = write::Unlock {
            message_address: 0,
            attention_mask: 0x101,
            path: lock_path.iter().map(|v| v.to_vec()).collect(),
            host_file: file as i32,
            txn,
            lock_type: write::LOCK_WRITE,
        };
        s.perform(&write::build_unlock(2, file, &req)).unwrap();
    };
    let flush = |s: &mut Session, file: u32| {
        s.perform_reply(&write::build_transact_info(
            3,
            file,
            &write::TransactInfo {
                files: Vec::new(),
                txn: 0,
            },
        ))
        .unwrap();
        s.take_notifications()
    };
    unlock(&mut prelogin, pre_file, 9);
    assert!(flush(&mut admin, admin_file).is_empty());
    unlock(&mut clerk, clerk_file, 9);
    assert!(flush(&mut admin, admin_file).is_empty());
    assert!(matches!(
        admin.lock(lock_path, 9, false).unwrap(),
        write::LockReply::Granted { .. }
    ));
    unlock(&mut admin, admin_file, 8); // A different transaction cannot release or notify.
    assert!(flush(&mut clerk, clerk_file).is_empty());
    unlock(&mut admin, admin_file, 9);
    assert_eq!(flush(&mut clerk, clerk_file).len(), 1);
    unlock(&mut admin, admin_file, 9); // The lock is already gone.
    assert!(flush(&mut clerk, clerk_file).is_empty());
    assert!(flush(&mut prelogin, pre_file).is_empty());

    // Pending and subsequent events are not delivered after logout; held
    // locks are released, including when the connection later logs in again.
    assert!(matches!(
        clerk.lock(lock_path, 10, false).unwrap(),
        write::LockReply::Granted { .. }
    ));
    let other_path: &[&[u8]] = &[&table, &[5], &[2]];
    assert!(matches!(
        admin.lock(other_path, 12, false).unwrap(),
        write::LockReply::Granted { .. }
    ));
    let queued = write::Unlock {
        message_address: 0,
        attention_mask: 0x101,
        path: other_path.iter().map(|v| v.to_vec()).collect(),
        host_file: admin_file as i32,
        txn: 12,
        lock_type: write::LOCK_WRITE,
    };
    admin
        .perform(&write::build_unlock(4, admin_file, &queued))
        .unwrap();
    let mut header = om_rctransaction::Header::new(Opcode::Logout as u8);
    header.file_id = clerk_file;
    clerk
        .perform(&om_rctransaction::Builder::with_header(header).finish())
        .unwrap();
    assert!(clerk.take_notifications().is_empty());
    assert!(matches!(
        admin.lock(lock_path, 11, false).unwrap(),
        write::LockReply::Granted { .. }
    ));
    unlock(&mut admin, admin_file, 11);
    assert!(flush(&mut clerk, clerk_file).is_empty());
    assert!(clerk.request_notifications().is_err());
    assert_eq!(login(&mut clerk, CLERK, CLERK_PASSWORD), 0);
    unlock(&mut clerk, clerk_file, 10);
    assert!(flush(&mut admin, admin_file).is_empty());

    prelogin.disconnect().unwrap();
    admin.disconnect().unwrap();
    clerk.disconnect().unwrap();
    assert!(handle.shutdown(Duration::from_secs(2)));
    handle.close_connections();
    assert!(thread.join().unwrap().is_ok());
}

/// A session keeps the privileges its Login gave it, as FileMaker keeps a
/// client's until it opens the file again (Claris's help, "Managing
/// security"): an account moved to another set over the wire, or deleted,
/// changes what its next Login gets, never what a session already has.
#[test]
fn a_session_keeps_the_privileges_it_logged_in_with() {
    use om_server::{HostedFile, Identity, Server, ServerConfig, ServerIdentity};

    let read_only_access = |root: &mut OwnedNode| {
        let set: &[u8] = &[3];
        root.put(
            &[&[23], &[2], &[5], set, &[16]],
            om_scripts::text_node(&[], "[Read-Only Access]"),
        );
        for (member, value) in [(2u8, [0x10u8, 0x03]), (0x0a, [0x03, 0x04]), (0x0c, [0, 0])] {
            root.put(
                &[&[23], &[2], &[5], set, &[member]],
                OwnedNode::leaf(Vec::new(), value.to_vec()),
            );
        }
        for (catalog, quad) in [(0x04u8, 0xdeu8), (0x11, 0xd6), (0x21, 0xd6)] {
            root.put(
                &[&[23], &[2], &[5], set, &[9], &[catalog]],
                OwnedNode::leaf(Vec::new(), vec![quad]),
            );
        }
    };
    let folder = Folder::new("session-privileges");
    let mut root = fixture::tree(&[], true);
    fixture::add_local_accounts(&mut root);
    fixture::put_data_entry_only(&mut root);
    read_only_access(&mut root);
    let path = folder.0.join(FILE);
    fixture::write_tree(&path, &root);
    let config = ServerConfig {
        identity: ServerIdentity {
            name: "synthetic".into(),
            version_string: "test".into(),
            server_id: "0123456789abcdef0123456789abcdef".into(),
            web_service_settings: String::new(),
        },
        files: vec![HostedFile {
            name: FILE.into(),
            path,
        }],
        insecure_files: false,
        record_dir: None,
    };
    let identity = Identity::self_signed(vec!["localhost".into()]).unwrap();
    let server = Server::bind("127.0.0.1:0", config, identity).unwrap();
    let port = server.local_addr().unwrap().port();
    let handle = server.handle().unwrap();
    let thread = std::thread::spawn(move || server.serve());
    let open = |name: &str, password: &str| {
        let c = client();
        let mut s = Session::connect(
            "127.0.0.1",
            port,
            &c.cert,
            &c.key,
            "127.0.0.1",
            &MachineInfo::default(),
        )
        .unwrap();
        s.open_file("127.0.0.1", FILE).unwrap();
        assert_eq!(s.fetch_internal_creation().unwrap(), INTERNAL_CREATION);
        let code = relogin(&mut s, name, password);
        (s, code)
    };
    fn relogin(s: &mut Session, name: &str, password: &str) -> u16 {
        s.login(
            name,
            password,
            INTERNAL_CREATION,
            &LoginEnvironment::for_remote_file("127.0.0.1", FILE),
        )
        .unwrap()
        .error_code
    }
    let table = uint_segment(TABLE);
    let mut txn = 20;
    let mut creates = |s: &mut Session| {
        txn += 1;
        s.create_record(&table, txn).unwrap().status
    };
    let clerk_record: &[&[u8]] = &[&[23], &[1], &[5], &[3]];
    let (mut admin, code) = open(ACCOUNT, PASSWORD);
    assert_eq!(code, 0);
    let (mut clerk, code) = open(CLERK, CLERK_PASSWORD);
    assert_eq!(code, 0);
    assert_eq!(creates(&mut clerk), 0, "[Data Entry Only] creates records");

    // Admin moves Clerk to [Read-Only Access]: Clerk's session still
    // creates records until it logs in again, and then it may not.
    let mut moved = root.at(clerk_record).unwrap().clone();
    moved.put(&[&[0x0b]], OwnedNode::leaf(Vec::new(), vec![1, 3]));
    admin.upload_to_host(clerk_record, &moved).unwrap();
    assert_eq!(
        creates(&mut clerk),
        0,
        "the session keeps [Data Entry Only]"
    );
    assert_eq!(relogin(&mut clerk, CLERK, CLERK_PASSWORD), 0);
    assert_eq!(
        creates(&mut clerk),
        200,
        "[Read-Only Access] after the Login"
    );

    // Admin deletes Clerk: the session keeps [Read-Only Access] (a deleted
    // account's session gains nothing), and Clerk can no longer log in.
    let mut accounts = root.at(&clerk_record[..3]).unwrap().clone();
    accounts.remove(&[&[3]]).unwrap();
    admin.upload_to_host(&clerk_record[..3], &accounts).unwrap();
    assert_eq!(
        creates(&mut clerk),
        200,
        "a deleted account's session may not write"
    );
    assert_eq!(open(CLERK, CLERK_PASSWORD).1, 212);

    admin.disconnect().unwrap();
    clerk.disconnect().unwrap();
    assert!(handle.shutdown(Duration::from_secs(2)));
    handle.close_connections();
    assert!(thread.join().unwrap().is_ok());
}

/// The #156 fixture, kept out of git (`re/work/` is ignored): FileMaker Pro
/// 26.0.3's English Contacts starter with accounts `entry` ([Data Entry
/// Only]) and `reader` ([Read-Only Access]) added in Manage Security on a
/// Mac, nothing else changed. FileMaker's own behavior under them was not
/// measured; this proves OpenMaker reads accounts FileMaker wrote and
/// applies the limits session-identity.md 2.5 documents.
const BUILT_IN_SETS_FIXTURE: &str = "re/work/156-accounts/Contacts.fmp12";

#[test]
#[ignore = "requires re/work/156-accounts/Contacts.fmp12, the FileMaker-made fixture of #156 (not in git)"]
fn accounts_filemaker_put_in_the_built_in_sets_open_with_their_limits() {
    use om_data::host::security::{BuiltInSet, SetAccess};
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(BUILT_IN_SETS_FIXTURE);
    let folder = Folder::new("built-in-sets");
    let path = folder.0.join("Contacts.fmp12");
    std::fs::copy(&fixture, &path).unwrap();
    let client = client();
    let host = LocalHost::open(&path, &client.cert).unwrap();
    // The Contacts table: a new record is reserved under [Data Entry Only]
    // and refused with 200 under [Read-Only Access].
    let contacts = uint_segment(0x8002);
    for (account, set, status) in [
        ("entry", BuiltInSet::DataEntryOnly, 0),
        ("reader", BuiltInSet::ReadOnlyAccess, 200),
        ("Admin", BuiltInSet::FullAccess, 0),
    ] {
        let password = if account == "Admin" { "" } else { account };
        let mut session = connect(&host, &client).unwrap();
        session.open_file("127.0.0.1", host.file_name()).unwrap();
        let creation = session.fetch_internal_creation().unwrap();
        let environment = LoginEnvironment::for_remote_file("127.0.0.1", host.file_name());
        let reply = session
            .login(account, password, creation, &environment)
            .unwrap();
        assert_eq!(reply.error_code, 0, "{account}");
        assert_eq!(
            host.authenticated_account(&reply.result_key).set_access,
            Some(SetAccess::BuiltIn(set)),
            "{account}"
        );
        let created = session.create_record(&contacts, 7).unwrap();
        assert_eq!(created.status, status, "{account}: {created:?}");
        session.disconnect().unwrap();
    }
    assert!(host.close());
}
