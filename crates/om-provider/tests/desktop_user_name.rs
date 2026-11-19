//! Desktop identity consumers against synthetic files on verified loopback TLS.
//! Each environment case runs in its own process, before Machine's OnceLock.
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
#[path = "common/related.rs"]
mod related;

use std::collections::BTreeMap;
use std::process::Command;

use fixture::{ACCOUNT, LAYOUT, NAME, OCCURRENCE, TABLE};
use om_data::host::machine;
use om_data::host::{CalcNeed, SessionInfo};
use om_data::runtime::{ServerScriptCall, TransactionOptions};
use om_data::{DataProvider, FieldChanges, RecordPage, ScriptWrite};
use om_hbam::OwnedNode;
use om_provider::NativeProvider;
use om_rctransaction::Reader;

const ROOT: &str = "Root.fmp12";
const RELATED: &str = "Related.fmp12";
const REMOTE: u64 = 0xd0_0040;
const BY_IDENTITY: u64 = 0xd0_0041;
const IDENTITY: u64 = 90;
const CREATED_USER: u64 = 91;
const MODIFIED_USER: u64 = 92;
const ENTRY_CALC: u64 = 93;
const MODIFIED_ACCOUNT: u64 = 94;
const REMOTE_IDENTITY: u64 = 95;
const REPORT: &str = "Get ( UserName ) & \"|\" & Get ( AccountName )";
const CASE: &str = "OPENMAKER_DESKTOP_NAME_TEST_CASE";

/// Never print the machine's fallback name, even on assertion failure.
fn isolated(test: &str, check: impl FnOnce(&str)) {
    if let Ok(case) = std::env::var(CASE) {
        let name = machine::user_name();
        match case.as_str() {
            "override" => assert!(name == "Synthetic Desktop User"),
            "empty" => assert!(name.is_empty()),
            "fallback" => {}
            _ => panic!("unknown isolated case"),
        }
        // Consumers must reuse the cached value, including empty, rather
        // than reading the override again. No server threads exist yet.
        std::env::set_var("OPENMAKER_USER_NAME", "Changed after Machine was cached");
        assert!(SessionInfo::for_this_machine().user_name == name);
        check(&name);
        return;
    }
    for case in ["override", "empty", "fallback"] {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", test, "--nocapture"])
            .env(CASE, case);
        match case {
            "override" => command.env("OPENMAKER_USER_NAME", "Synthetic Desktop User"),
            "empty" => command.env("OPENMAKER_USER_NAME", ""),
            _ => command.env_remove("OPENMAKER_USER_NAME"),
        };
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{test}, {case}:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn files(name: &str) -> Vec<(&'static str, OwnedNode)> {
    let script = fixture::script(1, "Server identity", vec![fixture::exit_with(REPORT)]);
    let mut root = fixture::tree(&[script], true);
    let mut child = fixture::tree(&[], true);
    let mut names = fixture::names();
    names.add_occurrence(REMOTE, "Remote Tasks", TABLE);
    for (id, name) in [
        (IDENTITY, "Identity"),
        (CREATED_USER, "CreatedUser"),
        (MODIFIED_USER, "ModifiedUser"),
        (ENTRY_CALC, "EntryIdentity"),
        (MODIFIED_ACCOUNT, "ModifiedAccount"),
        (REMOTE_IDENTITY, "RemoteIdentity"),
    ] {
        names.add_field(TABLE, id, name);
    }
    for tree in [&mut root, &mut child] {
        for (id, name, kind, variable, options) in [
            (IDENTITY, "Identity", 2, 0, 0x2_0020),
            (CREATED_USER, "CreatedUser", 1, 3, 1),
            (MODIFIED_USER, "ModifiedUser", 1, 8, 1),
            (ENTRY_CALC, "EntryIdentity", 1, 0, 8),
            (MODIFIED_ACCOUNT, "ModifiedAccount", 1, 9, 1),
        ] {
            fixture::add_field(tree, TABLE, id, name, kind, 1, variable, options, 0);
        }
        for id in [IDENTITY, ENTRY_CALC] {
            fixture::add_formula(tree, &names, (TABLE, OCCURRENCE), id, 5, REPORT);
        }
    }
    related::reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, NAME), 0, (REMOTE, NAME));
    related::reference(&mut root, (BY_IDENTITY, "Tasks by identity"), 1, "Related");
    fixture::add_relationship(
        &mut root,
        10,
        (OCCURRENCE, IDENTITY),
        0,
        (BY_IDENTITY, ENTRY_CALC),
    );
    // One stored target for the provider's calculation-data relationship
    // path. The source key is an unstored Get(UserName)/Get(AccountName).
    child.put(
        &[
            &om_hbam::slot::uint_segment(TABLE),
            &[5],
            &om_hbam::serial::encode(1),
            &om_hbam::slot::uint_segment(ENTRY_CALC),
        ],
        om_scripts::text_node(&[], &format!("{name}|{ACCOUNT}")),
    );
    fixture::add_field(
        &mut root,
        TABLE,
        REMOTE_IDENTITY,
        "RemoteIdentity",
        2,
        1,
        0,
        0x2_0020,
        0,
    );
    fixture::add_formula(
        &mut root,
        &names,
        (TABLE, OCCURRENCE),
        REMOTE_IDENTITY,
        5,
        "Remote Tasks::Identity",
    );
    vec![(ROOT, root), (RELATED, child)]
}

fn text<'a>(page: &'a RecordPage, id: i64, field: &str) -> &'a str {
    let column = page.fields.iter().position(|f| f.name == field).unwrap();
    page.records.iter().find(|r| r.id == id).unwrap().values[column]
        .as_deref()
        .unwrap_or_default()
}

#[test]
fn root_and_related_login_use_cached_machine_name() {
    isolated("root_and_related_login_use_cached_machine_name", |name| {
        let host = related::Host::new("desktop-login-name", files(name));
        let mut provider = host.open(ROOT);
        provider.calc_schema().unwrap();
        let logins = host.captures.with_opcode(om_protocol::Opcode::Login);
        assert_eq!(logins.len(), 2);
        for file in [ROOT, RELATED] {
            let (_, transaction) = logins.iter().find(|(f, _)| f == file).unwrap();
            let mut reader = Reader::parse(transaction).unwrap();
            reader.pop_u32().unwrap();
            reader.pop_u8().unwrap();
            assert!(
                reader.pop_string().unwrap() == name,
                "{file}: Login display name"
            );
            assert_eq!(
                reader.pop_string().unwrap(),
                format!("fmnet:/127.0.0.1/{file}")
            );
        }
        // The protocol's generic environment remains caller independent.
        assert_eq!(
            om_protocol::LoginEnvironment::for_remote_file("host", ROOT).session_name,
            "openmaker"
        );
        provider.close();
    });
}

#[test]
fn provider_calculations_use_cached_machine_name() {
    isolated("provider_calculations_use_cached_machine_name", |name| {
        let host = related::Host::new("desktop-calculation-name", files(name));
        let mut provider = host.open(ROOT);
        let expected = format!("{name}|{ACCOUNT}");
        let page = provider.records(LAYOUT as u32, 0, 100).unwrap();
        for id in [1, 2] {
            for field in ["Identity", "RemoteIdentity"] {
                assert!(
                    text(&page, id, field) == expected,
                    "{field}: calculation identity"
                );
            }
        }
        let data = provider
            .calc_data(&[CalcNeed::Related {
                from: OCCURRENCE,
                record: 1,
                to: BY_IDENTITY,
            }])
            .unwrap();
        assert_eq!(&*data.related[&(OCCURRENCE, 1, BY_IDENTITY)], &[1]);
        provider.close();
    });
}

fn check_entry(provider: &mut NativeProvider, id: i64, name: &str) {
    let page = provider.records(LAYOUT as u32, 0, 100).unwrap();
    for field in ["CreatedUser", "ModifiedUser"] {
        assert!(
            text(&page, id, field) == name,
            "{field}: auto-entry identity"
        );
    }
    assert!(text(&page, id, "EntryIdentity") == format!("{name}|{ACCOUNT}"));
    assert_eq!(text(&page, id, "Creator"), ACCOUNT);
    assert_eq!(text(&page, id, "ModifiedAccount"), ACCOUNT);
}

#[test]
fn ordinary_scripted_and_transaction_auto_entry_use_cached_machine_name() {
    isolated(
        "ordinary_scripted_and_transaction_auto_entry_use_cached_machine_name",
        |name| {
            let host = related::Host::new("desktop-auto-entry-name", files(name));
            let mut provider = host.open(ROOT);
            // An existing record has no creation user: editing must populate
            // its modification user without inventing a creator.
            provider
                .update_record(
                    LAYOUT as u32,
                    1,
                    FieldChanges::from([("Name".into(), Some("Changed".into()))]),
                )
                .unwrap();
            let page = provider.records(LAYOUT as u32, 0, 100).unwrap();
            assert!(text(&page, 1, "CreatedUser").is_empty());
            assert!(text(&page, 1, "ModifiedUser") == name);
            assert_eq!(text(&page, 1, "ModifiedAccount"), ACCOUNT);
            for transaction in [false, true] {
                for scripted in [false, true] {
                    if transaction {
                        provider
                            .begin_transaction(TransactionOptions::default())
                            .unwrap();
                    }
                    let write = |record, value: &str| ScriptWrite {
                        layout: LAYOUT as u32,
                        record,
                        cells: BTreeMap::from([(NAME, Some(value.into()))]),
                        ..ScriptWrite::default()
                    };
                    let created = if scripted {
                        provider.write_record(&write(None, "New")).unwrap()
                    } else {
                        provider
                            .create_record(
                                LAYOUT as u32,
                                FieldChanges::from([("Name".into(), Some("New".into()))]),
                            )
                            .unwrap()
                    };
                    if transaction {
                        provider.commit_transaction(false).unwrap();
                    }
                    check_entry(&mut provider, created.id, name);
                    if transaction {
                        provider
                            .begin_transaction(TransactionOptions::default())
                            .unwrap();
                    }
                    if scripted {
                        provider
                            .write_record(&write(Some(created.id), "Edited"))
                            .unwrap();
                    } else {
                        provider
                            .update_record(
                                LAYOUT as u32,
                                created.id,
                                FieldChanges::from([("Name".into(), Some("Edited".into()))]),
                            )
                            .unwrap();
                    }
                    if transaction {
                        provider.commit_transaction(false).unwrap();
                    }
                    check_entry(&mut provider, created.id, name);
                }
            }
            provider.close();
        },
    );
}

#[test]
fn server_scripts_keep_account_identity() {
    isolated("server_scripts_keep_account_identity", |name| {
        let host = related::Host::new("server-account-name", files(name));
        let mut provider = host.open(ROOT);
        let result = provider
            .perform_script_on_server(&ServerScriptCall {
                file: 0,
                script: 1,
                parameter: None,
                wait: true,
            })
            .unwrap();
        assert_eq!(result.error.0, 0);
        let Some(om_data::calc::Value::Text(text)) = result.result else {
            panic!("expected the server's identity report");
        };
        assert_eq!(text.into_string(), format!("{ACCOUNT}|{ACCOUNT}"));
        provider.close();
    });
}

#[test]
fn opened_desktop_files_use_the_cached_display_name() {
    isolated("opened_desktop_files_use_the_cached_display_name", |name| {
        let host = related::Host::new("desktop-peer-name", files(name));
        let mut root = host.open(ROOT);
        root.calc_schema().unwrap();
        let mut peer = root.open_peer(RELATED).unwrap();
        let page = peer.records(LAYOUT as u32, 0, 100).unwrap();
        assert!(text(&page, 1, "Identity") == format!("{name}|{ACCOUNT}"));
        let created = peer
            .create_record(
                LAYOUT as u32,
                FieldChanges::from([("Name".into(), Some("Created in peer".into()))]),
            )
            .unwrap();
        let page = peer.records(LAYOUT as u32, 0, 100).unwrap();
        for field in ["CreatedUser", "ModifiedUser"] {
            assert!(
                text(&page, created.id, field) == name,
                "peer auto-entry display name"
            );
        }
        assert_eq!(text(&page, created.id, "Creator"), ACCOUNT);
        assert_eq!(root.records(LAYOUT as u32, 0, 100).unwrap().total, 2);
        // Root's and its related Related's: the script's Related holds that
        // open of the file, on the same connection, with no Login of its
        // own (#1011).
        let logins = host.captures.with_opcode(om_protocol::Opcode::Login);
        assert!(logins.len() >= 2);
        for (file, transaction) in &logins {
            let mut reader = Reader::parse(transaction).unwrap();
            reader.pop_u32().unwrap();
            reader.pop_u8().unwrap();
            assert!(
                reader.pop_string().unwrap() == name,
                "desktop Login display name"
            );
            assert_eq!(
                reader.pop_string().unwrap(),
                format!("fmnet:/127.0.0.1/{file}")
            );
        }
        peer.close();
        root.close();
    });
}

/// A write's auto-entered formulas read what its operation supplies
/// (#187, `om_data::rules::WriteContext`): `Get ( LayoutNumber )` is the
/// number the window gives the layout written through,
/// `Get ( CurrentPrivilegeSetName )` the session's set for a user's write,
/// and `Get ( CurrentHostTimestamp )` the host's clock from one op82. A
/// script's write reads the set its step gave (`ScriptWrite::privilege_set`,
/// [Full Access] in a step running with full access); one that gave none is
/// refused rather than given the account's set.
#[test]
fn a_write_answers_its_layout_privilege_set_and_host_clock() {
    const CONTEXT: u64 = 96;
    const HOST_CLOCK: u64 = 97;
    let mut tree = fixture::tree(&[], true);
    let mut names = fixture::names();
    names.add_field(TABLE, CONTEXT, "Context");
    names.add_field(TABLE, HOST_CLOCK, "HostClock");
    fixture::add_field(&mut tree, TABLE, CONTEXT, "Context", 1, 1, 0, 8, 0);
    fixture::add_field(&mut tree, TABLE, HOST_CLOCK, "HostClock", 1, 5, 0, 8, 0);
    let report = "Get ( LayoutNumber ) & \"|\" & Get ( CurrentPrivilegeSetName )";
    fixture::add_formula(&mut tree, &names, (TABLE, OCCURRENCE), CONTEXT, 5, report);
    let clock = "Get ( CurrentHostTimestamp )";
    fixture::add_formula(&mut tree, &names, (TABLE, OCCURRENCE), HOST_CLOCK, 5, clock);
    let host = related::Host::new("write-context", vec![(ROOT, tree)]);
    let mut provider = host.open(ROOT);
    let layout = LAYOUT as u32;
    let number = provider
        .layouts()
        .unwrap()
        .iter()
        .position(|listed| listed.id == layout)
        .unwrap()
        + 1;
    let clocks = || {
        host.captures
            .with_opcode(om_protocol::Opcode::HostTimeStamp)
            .len()
    };
    let before = clocks();
    let record = provider.create_record(layout, FieldChanges::new()).unwrap();
    assert_eq!(clocks(), before + 1, "one op82 for the write");
    let page = provider.records(layout, 0, 100).unwrap();
    assert_eq!(
        text(&page, record.id, "Context"),
        format!("{number}|[Full Access]")
    );
    let stamp = text(&page, record.id, "HostClock");
    assert!(
        stamp
            .split_once(' ')
            .is_some_and(|(date, time)| { date.split('/').count() == 3 && time.contains(':') }),
        "{stamp}"
    );
    let scripted = provider
        .write_record(&ScriptWrite {
            layout,
            privilege_set: Some("[Full Access]".into()),
            ..ScriptWrite::default()
        })
        .unwrap();
    let page = provider.records(layout, 0, 100).unwrap();
    assert_eq!(
        text(&page, scripted.id, "Context"),
        format!("{number}|[Full Access]")
    );
    let refused = provider
        .write_record(&ScriptWrite {
            layout,
            ..ScriptWrite::default()
        })
        .unwrap_err();
    assert!(
        refused.message.contains("Get ( CurrentPrivilegeSetName )"),
        "{refused:?}"
    );
    provider.close();
}
