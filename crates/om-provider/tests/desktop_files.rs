//! Independent hosted desktop sessions over the existing synthetic TLS server.
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
#[path = "common/related.rs"]
mod related;
use om_data::{runtime::TransactionOptions, DataProvider};
use related::{reference, reference_with, Host};

#[test]
fn desktop_peer_inherits_the_endpoint_and_identity_and_owns_its_state() {
    let mut root = fixture::tree(&[], true);
    let peer = fixture::tree(&[], true);
    let mut denied = fixture::tree(&[], true);
    denied.put(
        &[&[23], &[1], &[5], &[2], &[16]],
        om_scripts::text_node(&[], "Different account"),
    );
    reference(&mut root, (0xd00040, "Peer rows"), 0x8001, "Peer");
    let host = Host::new(
        "desktop-owned-peer",
        vec![
            ("Root.fmp12", root),
            ("Peer.fmp12", peer),
            ("Denied.fmp12", denied),
        ],
    );
    let mut root = host.open("Root.fmp12");
    let sources = root.database_sources().unwrap();
    assert_eq!(sources.len(), 1);
    assert!(root.references_peer("Peer").unwrap());
    assert!(!root.references_peer("Unrelated").unwrap());
    assert_eq!(sources[0].id, 129);
    assert_eq!(sources[0].name, "Peer");
    assert_eq!(
        sources[0].paths[0]
            .same_host_name("127.0.0.1", host.port)
            .as_deref(),
        Some("Peer.fmp12")
    );
    root.use_window(31);
    let layout = root.layouts().unwrap()[0].id;
    let before = root.records(layout, 0, 2).unwrap();
    root.begin_transaction(TransactionOptions::default())
        .unwrap();
    let mut peer = root.open_peer("Peer.fmp12").unwrap();
    assert_eq!(peer.server_port(), host.port);
    assert_eq!(peer.session_account().as_deref(), Some(fixture::ACCOUNT));
    assert_eq!(peer.account_privileges(), root.account_privileges());
    peer.use_window(41);
    assert!(!peer.layouts().unwrap().is_empty());
    assert_eq!(root.records(layout, 0, 2).unwrap(), before);
    peer.close();
    assert_eq!(root.records(layout, 0, 2).unwrap(), before);
    root.revert_transaction().unwrap();
    assert!(root.open_peer("Absent.fmp12").is_err());
    assert_eq!(
        root.open_peer("Denied.fmp12").err().unwrap().code,
        Some(212)
    );
    assert_eq!(root.records(layout, 0, 2).unwrap(), before);
    root.close();
}

/// A script's file reference reads a data source's stored `fmnet` path as
/// the related files do (#759): one naming this host and port, stored as
/// FileMaker Pro 26.0.3 stores it, references the file; one to another port
/// of the same host does not.
#[test]
fn a_stored_fmnet_path_to_this_host_references_its_file() {
    let host = Host::naming_its_port("desktop-fmnet-peer", |port| {
        let mut root = fixture::tree(&[], true);
        let here = format!("\u{E000}127.0.0.1:{port}\u{E000}");
        let elsewhere = format!("\u{E000}127.0.0.1:{}\u{E000}", port.wrapping_add(1));
        reference_with(
            &mut root,
            (0xd00040, "Peer rows"),
            0x8001,
            "Peer",
            &[("fmnet", &here, "Peer")],
        );
        reference_with(
            &mut root,
            (0xd00041, "Other rows"),
            0x8002,
            "Other",
            &[("fmnet", &elsewhere, "Other")],
        );
        vec![
            ("Root.fmp12", root),
            ("Peer.fmp12", fixture::tree(&[], true)),
            ("Other.fmp12", fixture::tree(&[], true)),
        ]
    });
    let mut root = host.open("Root.fmp12");
    assert!(root.references_peer("Peer").unwrap());
    assert!(!root.references_peer("Other").unwrap());
    root.close();
}

/// A file on another host or port (#976), named `fmnet:/host:port/Name`,
/// whose files have a password of their own. Root's relationship to it
/// offered this file's account there once when Root opened, and it was
/// refused: a script's open of it sends nothing more and asks
/// (`peer_refused`), and this file's own password typed for it is the same
/// refusal, not sent again. The account typed for it opens the file on
/// that host's port, and that host's other files with it, without asking.
/// The file of the same name on this host is another file, opened with
/// this file's account. Root's relationships hold the other host's file
/// (its data source's `fmnet` path), so Close File refuses that one, and
/// not this host's, by host, port and name.
#[test]
fn a_peer_on_another_host_is_offered_the_files_account_once_then_asks() {
    let theirs = || {
        let mut file = fixture::tree(&[], true);
        fixture::put_account(&mut file, 2, "other host's own", None, 1);
        file
    };
    let other = Host::new(
        "peer-other",
        vec![("Peer.fmp12", theirs()), ("Other.fmp12", theirs())],
    );
    let mut root_tree = fixture::tree(&[], true);
    let there = format!("\u{E000}127.0.0.1:{}\u{E000}", other.port);
    reference_with(
        &mut root_tree,
        (0xd00042, "Remote rows"),
        0x8003,
        "Remote",
        &[("fmnet", &there, "Peer")],
    );
    let host = Host::new(
        "peer-here",
        vec![
            ("Root.fmp12", root_tree),
            ("Peer.fmp12", fixture::tree(&[], true)),
        ],
    );
    let mut root = host.provider_trusting(&other);
    root.open(&host.connection("Root.fmp12")).unwrap();
    // Its related files open with its schema.
    root.calc_schema().unwrap();
    let name = om_data::file_refs::fmnet_name("127.0.0.1", other.port, "Peer");
    assert!(
        root.references_peer(&name).unwrap(),
        "held by Root's relationships"
    );
    assert!(
        !root.references_peer("Peer.fmp12").unwrap(),
        "this host's is not"
    );
    let unrelated = om_data::file_refs::fmnet_name("127.0.0.1", other.port, "Other");
    assert!(!root.references_peer(&unrelated).unwrap());
    let logins = || other.captures.opens_and_logins().1;
    assert_eq!(logins(), [("Peer.fmp12".to_owned(), 1)].into(), "the offer");
    let error = root.open_peer(&name).map(|_| ()).unwrap_err();
    assert_eq!(error.code, Some(212), "{error:?}");
    assert!(root.peer_refused(&name, None));
    // That host's other file is not offered it either.
    let error = root.open_peer(&unrelated).map(|_| ()).unwrap_err();
    assert_eq!(error.code, Some(212), "{error:?}");
    assert!(root.peer_refused(&unrelated, None));
    // This file's own password, typed for it, is the refusal remembered.
    let error = root
        .open_peer_as(&name, fixture::ACCOUNT, fixture::PASSWORD)
        .map(|_| ())
        .unwrap_err();
    assert_eq!(error.code, Some(212), "{error:?}");
    assert!(root.peer_refused(&name, Some((fixture::ACCOUNT, fixture::PASSWORD))));
    assert!(!root.peer_refused(&name, Some((fixture::ACCOUNT, "other host's own"))));
    assert_eq!(
        logins(),
        [("Peer.fmp12".to_owned(), 1)].into(),
        "no second Login"
    );
    let mut there = root
        .open_peer_as(&name, fixture::ACCOUNT, "other host's own")
        .unwrap();
    assert_eq!(there.server_port(), other.port);
    // The account that host admitted opens its other file, unasked.
    let mut other_file = root.open_peer(&unrelated).unwrap();
    assert_eq!(other_file.server_port(), other.port);
    let mut here = root.open_peer("Peer.fmp12").unwrap();
    assert_eq!(here.server_port(), host.port);
    assert_eq!(
        logins(),
        [("Other.fmp12".to_owned(), 1), ("Peer.fmp12".to_owned(), 2)].into()
    );
    let (_, logins) = host.captures.opens_and_logins();
    assert_eq!(logins.get("Peer.fmp12"), Some(&1), "{logins:?}");
    there.close();
    other_file.close();
    here.close();
    root.close();
}

/// A script's open of a file on another host that nothing of Root's
/// relates to: Root's account is offered there once, as FileMaker Pro
/// offers it (network-protocol.md 5.5). A file that admits it opens with
/// no account typed for it; one with a password of its own refuses it
/// once, and asks, with no second Login. A host whose certificate the
/// client does not trust gets no Login, and the step does not ask.
#[test]
fn a_script_offers_another_host_the_files_account_once() {
    let mut guarded = fixture::tree(&[], true);
    fixture::put_account(&mut guarded, 2, "other host's own", None, 1);
    let other = Host::new(
        "offer-other",
        vec![
            ("Peer.fmp12", fixture::tree(&[], true)),
            ("Guarded.fmp12", guarded),
        ],
    );
    let host = Host::new("offer-here", vec![("Root.fmp12", fixture::tree(&[], true))]);
    let name = |file: &str| om_data::file_refs::fmnet_name("127.0.0.1", other.port, file);
    let logins = || other.captures.opens_and_logins().1;
    // Not trusting the other host: no Login, no account asked for.
    let mut untrusting = host.open("Root.fmp12");
    let error = untrusting.open_peer(&name("Peer")).map(|_| ()).unwrap_err();
    assert_ne!(error.code, Some(212), "{error:?}");
    assert!(!untrusting.peer_refused(&name("Peer"), None));
    assert!(logins().is_empty(), "{:?}", logins());
    untrusting.close();
    let mut root = host.provider_trusting(&other);
    root.open(&host.connection("Root.fmp12")).unwrap();
    let mut peer = root.open_peer(&name("Peer")).unwrap();
    assert_eq!(peer.server_port(), other.port);
    // Admitted there, the account goes to that host's other file too, as
    // the account that host admitted; refused, it is not sent again.
    for _ in 0..2 {
        let error = root.open_peer(&name("Guarded")).map(|_| ()).unwrap_err();
        assert_eq!(error.code, Some(212), "{error:?}");
        assert!(root.peer_refused(&name("Guarded"), None));
    }
    assert_eq!(
        logins(),
        [
            ("Guarded.fmp12".to_owned(), 1),
            ("Peer.fmp12".to_owned(), 1)
        ]
        .into()
    );
    peer.close();
    root.close();
}

/// The files a script opens on a host open in the session the client has
/// there, as FileMaker Pro opens a host's files on one connection (#1011,
/// network-protocol.md 5.5): Root's for files on its host; on the other
/// host a related file's, and then a file open there opens that host's
/// next file in its own (as the desktop routes it). The two hosts' files
/// stay on their own host's connection. One whose Login is refused closes
/// alone, and the others go on when Root closes first.
#[test]
fn files_a_script_opens_share_their_hosts_session() {
    let mut denied = fixture::tree(&[], true);
    fixture::put_account(&mut denied, 2, "its own", None, 1);
    let other = Host::new(
        "shared-other",
        vec![
            ("Related.fmp12", fixture::tree(&[], true)),
            ("Peer.fmp12", fixture::tree(&[], true)),
            ("Other.fmp12", fixture::tree(&[], true)),
        ],
    );
    let mut root_tree = fixture::tree(&[], true);
    let there = format!("\u{E000}127.0.0.1:{}\u{E000}", other.port);
    reference_with(
        &mut root_tree,
        (0xd00042, "Remote rows"),
        0x8003,
        "Remote",
        &[("fmnet", &there, "Related")],
    );
    let host = Host::new(
        "shared-here",
        vec![
            ("Root.fmp12", root_tree),
            ("Peer.fmp12", fixture::tree(&[], true)),
            ("Other.fmp12", fixture::tree(&[], true)),
            ("Denied.fmp12", denied),
        ],
    );
    let mut root = host.provider_trusting(&other);
    root.open(&host.connection("Root.fmp12")).unwrap();
    // Related, on the other host, opens with the schema.
    root.calc_schema().unwrap();
    let mut peer = root.open_peer("Peer.fmp12").unwrap();
    let mut other_file = root.open_peer("Other.fmp12").unwrap();
    let error = root.open_peer("Denied.fmp12").map(|_| ()).unwrap_err();
    assert_eq!(error.code, Some(212), "{error:?}");
    let on_other = |file: &str| om_data::file_refs::fmnet_name("127.0.0.1", other.port, file);
    let mut there = root.open_peer(&on_other("Peer")).unwrap();
    let mut there_too = there.open_peer(&on_other("Other")).unwrap();
    for file in [&there, &there_too] {
        assert_eq!(file.server_port(), other.port);
    }
    assert_eq!(peer.server_port(), host.port);
    assert_eq!(host.captures.connections(), 1, "one to this host");
    assert_eq!(other.captures.connections(), 1, "one to the other host");
    root.close();
    // Each reads its record from its host after Root closed: a record
    // download (op92) the host answers, not a catalog read before.
    for (file, name, host) in [
        (&mut peer, "Peer.fmp12", &host),
        (&mut other_file, "Other.fmp12", &host),
        (&mut there, "Peer.fmp12", &other),
        (&mut there_too, "Other.fmp12", &other),
    ] {
        let downloads = || {
            host.captures
                .row_downloads()
                .iter()
                .filter(|(file, _, _)| file == name)
                .count()
        };
        let before = downloads();
        let page = file.records(fixture::LAYOUT as u32, 0, 1).unwrap();
        assert_eq!(page.records[0].id, 1, "{name}");
        assert!(downloads() > before, "{name} read from its host");
        file.close();
    }
}

/// Read `file` until it knows its connection is lost (the host dropped
/// it); `false` when it does not within 10 seconds.
fn lose(file: &mut dyn DataProvider) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while file.connection_lost().is_none() && std::time::Instant::now() < deadline {
        let _ = file.records(fixture::LAYOUT as u32, 0, 1);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    file.connection_lost().is_some()
}

/// A file of the lost connection whose host refuses its account on the new
/// one (its password changed meanwhile) is not sent it again: Root's
/// relationship to it tried it once while Root reconnected and the refusal
/// is remembered for the session, so reopening the file there sends no
/// Login and fails with that refusal. Its window stays lost for its own
/// Reconnect; nothing is tried again.
#[test]
fn a_file_refused_on_the_new_connection_stays_lost_without_another_login() {
    let tree = || fixture::tree(&[], true);
    let mut root_tree = tree();
    reference(&mut root_tree, (0xd00040, "Peer rows"), 0x8001, "Peer");
    let host = Host::new(
        "reconnect-refused",
        vec![("Root.fmp12", root_tree), ("Peer.fmp12", tree())],
    );
    let mut root = host.open("Root.fmp12");
    root.calc_schema().unwrap();
    let mut peer = root.open_peer("Peer.fmp12").unwrap();
    let logins = || {
        host.captures
            .opens_and_logins()
            .1
            .get("Peer.fmp12")
            .copied()
            .unwrap_or(0)
    };
    assert_eq!(logins(), 1, "the script's Peer holds the relationship's");
    for client in host.handle.clients() {
        host.handle.disconnect_client(client.id);
    }
    assert!(lose(&mut root));
    assert!(peer.connection_lost().is_some());
    let mut changed = tree();
    fixture::put_account(&mut changed, 2, "changed meanwhile", None, 1);
    host.replace_file("Peer.fmp12", &changed);
    root.reopen(&host.connection("Root.fmp12")).unwrap();
    assert_eq!(logins(), 2, "the relationship's Login, refused");
    let refused = peer.reopen_on_successor().unwrap_err();
    assert_eq!(refused.code, Some(212), "{refused:?}");
    assert_eq!(logins(), 2, "the refusal remembered: no Login");
    assert!(peer.records(fixture::LAYOUT as u32, 0, 1).is_err());
    assert!(!matches!(peer.reopen_on_successor(), Ok(true)));
    assert_eq!(logins(), 2, "nothing tried again");
    peer.close();
    root.close();
}

/// After the host drops the connection its files share (#1011), one
/// Reconnect of Root's window reopens the other files of that connection
/// opened with the same account, with the account they hold (nothing more
/// typed): on the one new connection, each with its window's found set,
/// read from the host again. A file opened there with another account, and
/// a file on another host, stay lost for their own Reconnect.
#[test]
fn one_reconnect_reopens_the_files_that_shared_the_lost_connection() {
    use om_data::found::FoundSetChange;
    let tree = || fixture::tree(&[], true);
    let other = Host::new("reconnect-other", vec![("There.fmp12", tree())]);
    let host = Host::new(
        "reconnect-shared",
        vec![
            ("Root.fmp12", tree()),
            ("Peer.fmp12", tree()),
            ("Other.fmp12", tree()),
            ("Clerked.fmp12", tree()),
        ],
    );
    let layout = fixture::LAYOUT as u32;
    let mut root = host.provider_trusting(&other);
    root.open(&host.connection("Root.fmp12")).unwrap();
    let mut peer = root.open_peer("Peer.fmp12").unwrap();
    let mut other_file = root.open_peer("Other.fmp12").unwrap();
    let mut clerked = root
        .open_peer_as("Clerked.fmp12", fixture::CLERK, fixture::CLERK_PASSWORD)
        .unwrap();
    let name = om_data::file_refs::fmnet_name("127.0.0.1", other.port, "There");
    let mut there = root.open_peer(&name).unwrap();
    assert_eq!(host.captures.connections(), 1, "the files share Root's");
    peer.change_found_set(layout, FoundSetChange::Records(vec![2]))
        .unwrap();
    for server in [&host, &other] {
        for client in server.handle.clients() {
            server.handle.disconnect_client(client.id);
        }
    }
    assert!(lose(&mut root) && lose(there.as_mut()));
    for file in [&peer, &other_file, &clerked] {
        assert!(file.connection_lost().is_some(), "lost with Root's");
    }

    root.reopen(&host.connection("Root.fmp12")).unwrap();
    assert!(peer.reopen_on_successor().unwrap());
    assert!(other_file.reopen_on_successor().unwrap());
    assert!(!clerked.reopen_on_successor().unwrap(), "another account");
    assert!(!there.reopen_on_successor().unwrap(), "another host");
    assert!(clerked.connection_lost().is_some() && there.connection_lost().is_some());
    assert_eq!(host.captures.connections(), 2, "one new connection");
    let downloads = |file: &str| {
        host.captures
            .row_downloads()
            .iter()
            .filter(|(name, _, _)| name == file)
            .count()
    };
    let before = downloads("Peer.fmp12");
    let page = peer.records(layout, 0, 10).unwrap();
    assert_eq!(
        (page.total, page.records[0].id),
        (1, 2),
        "its found set kept"
    );
    assert!(downloads("Peer.fmp12") > before, "read from the host again");
    let before = downloads("Other.fmp12");
    assert!(!other_file.records(layout, 0, 1).unwrap().records.is_empty());
    assert!(downloads("Other.fmp12") > before);
    for file in [&mut peer, &mut other_file, &mut clerked, &mut there] {
        file.close();
    }
    root.close();
}

#[test]
fn desktop_peer_cancel_before_connect_keeps_the_caller_usable() {
    let host = Host::new(
        "desktop-cancel",
        vec![
            ("Root.fmp12", fixture::tree(&[], true)),
            ("Peer.fmp12", fixture::tree(&[], true)),
        ],
    );
    let mut root = host.open("Root.fmp12");
    let closes = root.close_requests().unwrap();
    closes.queued();
    let error = root.open_peer("Peer.fmp12").err().expect("canceled open");
    assert_eq!(error.code, Some(1));
    closes.handled();
    assert!(!root.layouts().unwrap().is_empty());
    root.close();
    let (opens, _) = host.captures.opens_and_logins();
    assert!(!opens.contains_key("Peer.fmp12"));
}

#[test]
fn desktop_and_related_readers_share_owning_file_globals_without_colliding_ids() {
    use om_data::{
        calc::{Ctx, Date, Locale},
        host::CalcNeed,
    };
    let mut root_tree = fixture::tree(&[], true);
    reference(&mut root_tree, (0xd00040, "Peer rows"), 1, "Peer");
    let host = Host::new(
        "desktop-related-globals",
        vec![
            ("Root.fmp12", root_tree),
            ("Peer.fmp12", fixture::tree(&[], true)),
        ],
    );
    let mut root = host.open("Root.fmp12");
    let schema = root.calc_schema().unwrap().unwrap();
    let table = schema.occurrence(0xd00040).unwrap().table;
    root.set_global(fixture::TABLE, fixture::SETTING, 1, "root override")
        .unwrap();
    let needs = [CalcNeed::Globals { table }];
    let before = root.calc_data(&needs).unwrap();
    let mut peer = root.open_peer("Peer.fmp12").unwrap();
    peer.set_global(fixture::TABLE, fixture::SETTING, 1, "peer override")
        .unwrap();
    let changed = root.calc_data(&needs).unwrap();
    let locale = Locale::en_us();
    let cx = Ctx::new(&locale, Date::from_ymd(2026, 1, 1).unwrap());
    assert_eq!(
        changed.globals[&table]
            .get(fixture::SETTING, 1)
            .unwrap()
            .to_text(&cx)
            .as_str(),
        "peer override"
    );
    assert_ne!(before.generation, changed.generation);
    let own = root
        .calc_data(&[CalcNeed::Globals {
            table: fixture::TABLE,
        }])
        .unwrap();
    assert_eq!(
        own.globals[&fixture::TABLE]
            .get(fixture::SETTING, 1)
            .unwrap()
            .to_text(&cx)
            .as_str(),
        "root override"
    );
    // Closing the desktop window leaves the relationship's file alive.
    peer.close();
    let mut reopened = root.open_peer("Peer.fmp12").unwrap();
    let values = reopened
        .calc_data(&[CalcNeed::Globals {
            table: fixture::TABLE,
        }])
        .unwrap();
    assert_eq!(
        values.globals[&fixture::TABLE]
            .get(fixture::SETTING, 1)
            .unwrap()
            .to_text(&cx)
            .as_str(),
        "peer override"
    );
    reopened.close();
    root.close();
    root.open(&host.connection("Root.fmp12")).unwrap();
    let schema = root.calc_schema().unwrap().unwrap();
    let table = schema.occurrence(0xd00040).unwrap().table;
    let fresh = root.calc_data(&[CalcNeed::Globals { table }]).unwrap();
    assert!(fresh.globals[&table].get(fixture::SETTING, 1).is_none());
    root.close();
}

#[test]
fn desktop_file_identity_preserves_an_embedded_file_extension() {
    use om_data::{
        calc::{Ctx, Date, Locale},
        host::CalcNeed,
    };
    let host = Host::new(
        "desktop-file-identity",
        vec![
            ("Root.fmp12", fixture::tree(&[], true)),
            ("Peer.fmp12", fixture::tree(&[], true)),
            ("Peer.fmp12.fmp12", fixture::tree(&[], true)),
        ],
    );
    let mut root = host.open("Root.fmp12");
    let mut plain = root.open_peer("Peer.fmp12").unwrap();
    let mut nested = root.open_peer("Peer.fmp12.fmp12").unwrap();
    plain
        .set_global(fixture::TABLE, fixture::SETTING, 1, "plain")
        .unwrap();
    nested
        .set_global(fixture::TABLE, fixture::SETTING, 1, "nested")
        .unwrap();
    let needs = [CalcNeed::Globals {
        table: fixture::TABLE,
    }];
    let locale = Locale::en_us();
    let cx = Ctx::new(&locale, Date::from_ymd(2026, 1, 1).unwrap());
    for (file, expected) in [(&mut plain, "plain"), (&mut nested, "nested")] {
        let values = file.calc_data(&needs).unwrap();
        assert_eq!(
            values.globals[&fixture::TABLE]
                .get(fixture::SETTING, 1)
                .unwrap()
                .to_text(&cx)
                .as_str(),
            expected
        );
    }
    plain.close();
    nested.close();
    root.close();
}
