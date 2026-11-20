//! Which related hosted files the desktop provider opens (external.rs):
//! each once, whatever references it, in a stable order, and on another
//! host only with an account that host admits. Synthetic Tasks files on
//! verified loopback TLS.

#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
#[path = "common/related.rs"]
mod related;

use fixture::{ACCOUNT, TABLE};
use om_data::DataProvider;
use om_hbam::slot::uint_segment;
use related::{reference, reference_with, Host};

/// The password the other host's files have for the account, where it is
/// not the opened file's.
const THEIRS: &str = "the other host's own";

/// Root references Alpha to Delta, a file the host does not have, Alpha
/// again under another spelling, and Denied, whose account list lacks the
/// Admin account. Alpha references Shared, Root and the missing file;
/// Beta and Delta reference Shared, Gamma references Denied, and Shared
/// references Alpha: shared files and cycles.
fn graph() -> Vec<(&'static str, om_hbam::OwnedNode)> {
    let names = [
        "Root", "Alpha", "Beta", "Gamma", "Delta", "Shared", "Denied",
    ];
    let mut trees: Vec<_> = names.iter().map(|_| fixture::tree(&[], true)).collect();
    let occurrence = |n: u64| 0xd1_0000 + n;
    for (source, file) in [
        (1, "Alpha"),
        (2, "Beta"),
        (3, "Gamma"),
        (4, "Delta"),
        (5, "Missing"),
        (6, "alpha.FMP12"),
        (7, "Denied"),
    ] {
        reference(
            &mut trees[0],
            (occurrence(source), &format!("Related {source}")),
            source,
            file,
        );
    }
    for (tree, source, file) in [
        (1, 1, "Shared"),
        (1, 2, "root.fmp12"),
        (1, 3, "Missing"),
        (2, 1, "Shared.fmp12"),
        (3, 1, "Denied"),
        (4, 1, "Shared"),
        (5, 1, "Alpha"),
    ] {
        reference(
            &mut trees[tree],
            (occurrence(source), &format!("Related {source}")),
            source,
            file,
        );
    }
    // Denied's only account is another one: its one refused Login must not
    // be retried through another reference, nor keep the others from
    // opening.
    trees[6].put(
        &[&[23], &[1], &[5], &[2], &[16]],
        om_scripts::text_node(&[], "Other account"),
    );
    let names: Vec<&'static str> = vec![
        "Root.fmp12",
        "Alpha.fmp12",
        "Beta.fmp12",
        "Gamma.fmp12",
        "Delta.fmp12",
        "Shared.fmp12",
        "Denied.fmp12",
    ];
    names.into_iter().zip(trees).collect()
}

#[test]
fn shared_files_and_cycles_open_once_with_stable_ids_and_failures_once() {
    let host = Host::new("graph", graph());
    let mut provider = host.provider();
    let mut previous = None;
    for pass in 1..=2 {
        provider.open(&host.connection("Root.fmp12")).unwrap();
        let schema = provider.calc_schema().unwrap().unwrap();
        let tables: Vec<_> = schema
            .tables()
            .map(|table| (table.id, table.name.clone()))
            .collect();
        assert_eq!(
            tables.len(),
            12,
            "Root and five related files, two tables each, each imported once: {tables:?}"
        );
        if let Some(previous) = &previous {
            assert_eq!(&tables, previous, "the same files import to the same ids");
        }
        previous = Some(tables);

        let warnings = provider.related_file_warnings().to_vec();
        for file in ["Missing", "Denied"] {
            let needle = format!("Related file “{file}” (data source “{file}” of “Root”): ");
            assert_eq!(
                warnings.iter().filter(|w| w.contains(&needle)).count(),
                1,
                "{warnings:?}"
            );
        }
        assert!(
            warnings.iter().all(|w| !w.contains(fixture::PASSWORD)),
            "{warnings:?}"
        );
        // Occurrences of the files that could not be opened stay
        // unresolved.
        for source in [5, 7] {
            assert!(schema.table_of(0xd1_0000 + source).is_none());
        }
        assert!(schema.table_of(0xd1_0001).is_some());

        let (opens, logins) = host.captures.opens_and_logins();
        for file in [
            "Root.fmp12",
            "Alpha.fmp12",
            "Beta.fmp12",
            "Gamma.fmp12",
            "Delta.fmp12",
            "Shared.fmp12",
            "Denied.fmp12",
        ] {
            assert_eq!(opens.get(file), Some(&pass), "{file}: one Open per open");
            assert_eq!(logins.get(file), Some(&pass), "{file}: one Login per open");
        }
        assert_eq!(opens.get("Missing.fmp12"), Some(&pass));
        assert!(!logins.contains_key("Missing.fmp12"));
        let clients = host.handle.clients();
        for name in ["Root", "Alpha", "Beta", "Gamma", "Delta", "Shared"] {
            let sessions = clients
                .iter()
                .flat_map(|client| &client.files)
                .filter(|file| {
                    file.name == format!("{name}.fmp12") && file.account.as_deref() == Some(ACCOUNT)
                })
                .count();
            assert_eq!(sessions, 1, "{name}");
        }
        provider.close();
    }
}

/// A data source's relative paths are tried in their order, as FileMaker
/// Pro searches them (#739): a path the host does not have is passed over,
/// and the first file it has is the data source's file. A file that would
/// not open for another reason (here the account refused) ends the search,
/// gets one Login whatever lists it, and its later paths are not tried. A
/// path naming the opened file binds to it without opening it again.
#[test]
fn a_data_source_tries_its_paths_until_the_host_has_one() {
    let mut root = fixture::tree(&[], true);
    let file = |name: &'static str| ("file", "", name);
    for (source, paths) in [
        (1, vec![file("Missing"), file("Second")]),
        (2, vec![file("Missing"), file("Denied"), file("Third")]),
        (3, vec![file("Denied")]),
        (4, vec![file("Gone"), file("Root")]),
    ] {
        reference_with(
            &mut root,
            (0xd0_0050 + source, &format!("Source {source}")),
            source,
            &format!("Source {source}"),
            &paths,
        );
    }
    let mut denied = fixture::tree(&[], true);
    denied.put(
        &[&[23], &[1], &[5], &[2], &[16]],
        om_scripts::text_node(&[], "Other account"),
    );
    let host = Host::new(
        "paths",
        vec![
            ("Root.fmp12", root),
            ("Second.fmp12", fixture::tree(&[], true)),
            ("Denied.fmp12", denied),
            ("Third.fmp12", fixture::tree(&[], true)),
        ],
    );
    let mut provider = host.open("Root.fmp12");
    let schema = provider.calc_schema().unwrap().unwrap();
    assert!(schema.table_of(0xd0_0051).is_some(), "Second's table");
    assert!(schema.table_of(0xd0_0052).is_none());
    assert!(schema.table_of(0xd0_0053).is_none());
    assert!(schema.table_of(0xd0_0054).is_some(), "Root's own table");
    let (opens, logins) = host.captures.opens_and_logins();
    for (file, expected_opens, expected_logins) in [
        ("Root.fmp12", 1, 1),
        ("Missing.fmp12", 1, 0),
        ("Second.fmp12", 1, 1),
        ("Denied.fmp12", 1, 1),
        ("Third.fmp12", 0, 0),
        ("Gone.fmp12", 1, 0),
    ] {
        assert_eq!(
            opens.get(file).copied().unwrap_or(0),
            expected_opens,
            "{file} Opens: {opens:?}"
        );
        assert_eq!(
            logins.get(file).copied().unwrap_or(0),
            expected_logins,
            "{file} Logins: {logins:?}"
        );
    }
    // One line about Denied, though two data sources reach it, saying
    // which file named it and that the account was refused.
    let warnings = provider.related_file_warnings();
    let denied: Vec<_> = warnings
        .iter()
        .filter(|w| w.starts_with("Related file “Denied” (data source “Source "))
        .collect();
    assert_eq!(denied.len(), 1, "{warnings:?}");
    assert!(
        denied[0].ends_with(
            " of “Root”): the host did not accept the account “Admin” and its password \
             for it (FileMaker error 212)."
        ),
        "{warnings:?}"
    );
    assert!(
        !warnings
            .iter()
            .any(|w| w.contains("Second") || w.contains("Gone")),
        "a data source that found its file reports none of the paths before it: {warnings:?}"
    );
    // Reading the schema again (as Manage Database does) rebuilds it
    // without the credentials: the files found are bound again, and
    // nothing is opened or logged in to.
    provider.database_schema().unwrap();
    let schema = provider.calc_schema().unwrap().unwrap();
    assert!(schema.table_of(0xd0_0051).is_some(), "Second's table again");
    assert!(
        schema.table_of(0xd0_0054).is_some(),
        "Root's own table again"
    );
    assert_eq!(host.captures.opens_and_logins(), (opens, logins));
    assert!(
        !provider
            .related_file_warnings()
            .iter()
            .any(|w| w.contains("was not opened")),
        "{:?}",
        provider.related_file_warnings()
    );
    provider.close();
}

/// Each warning says which file it is about and names a data source by its
/// name, never by an id (#741):
/// - two related files with a data source of the same name give two lines;
/// - an occurrence showing a table its file does not have is named, with
///   no cause claimed, and the file is the one its data source found, not
///   a path the host did not have; occurrences of one file and table share
///   a line, and the same missing table seen from two files gives two;
/// - a data source missing from the file's data sources, one there with
///   no name and no paths, a name that does not decode, an empty list of
///   paths, and one that does not decode or reassemble are each said to be
///   what they are; one without a readable name is named by the
///   occurrences that use it (#761).
#[test]
fn related_file_warnings_say_which_file_and_data_source_they_are_about() {
    // A folder on a host: a form OpenMaker does not follow.
    let elsewhere = [(
        "fmnet",
        "\u{E000}elsewhere.invalid\u{E000}Folder\u{E000}",
        "Remote",
    )];
    let occurrence = |tree: &mut om_hbam::OwnedNode, id: u64, name: &str, source, table| {
        fixture::add_occurrence(tree, id, name, TABLE);
        let mut definition = related::key(source);
        definition.extend(related::key(table));
        tree.put(
            &[&[3], &[17], &[5], &uint_segment(id), &[2]],
            om_hbam::OwnedNode::leaf(Vec::new(), definition),
        );
    };
    let source = |tree: &mut om_hbam::OwnedNode, id: u64, name: &str, paths: Vec<u8>| {
        tree.put(
            &[&[32], &[5], &uint_segment(id), &[0x10]],
            om_scripts::text_node(&[], name),
        );
        tree.put(
            &[&[32], &[5], &uint_segment(id), &[0x82]],
            om_hbam::OwnedNode::leaf(Vec::new(), paths),
        );
    };
    let mut root = fixture::tree(&[], true);
    reference(&mut root, (0xd0_0061, "Alpha Tasks"), 1, "Alpha");
    reference(&mut root, (0xd0_0062, "Beta Tasks"), 2, "Beta");
    // Tables Alpha does not have, once through a path the host lacks first.
    occurrence(&mut root, 0xd0_0063, "Old Tasks", 1, 0x80ff);
    reference_with(
        &mut root,
        (0xd0_0066, "Fallback Tasks"),
        4,
        "Fallback",
        &[("file", "", "Gone"), ("file", "", "Alpha")],
    );
    occurrence(&mut root, 0xd0_0067, "Gone Tasks", 4, 0x80ff);
    // A table Beta does not have, seen from Root and from Alpha.
    occurrence(&mut root, 0xd0_0068, "Root Missing", 2, 0x80fd);
    // Data source 3: not among the file's data sources. 7: there, with no
    // name and no paths. 5: an empty list. 6: one that does not decode.
    occurrence(&mut root, 0xd0_0064, "Lost Tasks", 3, TABLE);
    root.put(
        &[&[32], &[5], &uint_segment(7), &[0x01]],
        om_hbam::OwnedNode::leaf(Vec::new(), vec![1]),
    );
    occurrence(&mut root, 0xd0_006b, "Bare Tasks", 7, TABLE);
    occurrence(&mut root, 0xd0_006c, "Bare Notes", 7, TABLE);
    // 8: a name that does not decode (stored 0x54 unmasks to an SCSU tag
    // with no argument) and no paths. 9: a list of paths stored in a form
    // that does not reassemble.
    root.put(
        &[&[32], &[5], &uint_segment(8), &[0x10]],
        om_hbam::OwnedNode::leaf(Vec::new(), vec![0x54]),
    );
    occurrence(&mut root, 0xd0_006d, "Garbled Tasks", 8, TABLE);
    root.put(
        &[&[32], &[5], &uint_segment(9), &[0x10]],
        om_scripts::text_node(&[], "Chunked"),
    );
    root.put(
        &[&[32], &[5], &uint_segment(9), &[0x82]],
        om_hbam::OwnedNode::list(
            Vec::new(),
            vec![om_hbam::OwnedNode::leaf(vec![5], vec![1, 2, 3])],
        ),
    );
    occurrence(&mut root, 0xd0_006e, "Chunked Tasks", 9, TABLE);
    source(&mut root, 5, "Empty", 0u32.to_be_bytes().to_vec());
    occurrence(&mut root, 0xd0_0069, "Empty Tasks", 5, TABLE);
    source(&mut root, 6, "Broken", vec![0, 0, 0, 1, 9]);
    occurrence(&mut root, 0xd0_006a, "Broken Tasks", 6, TABLE);
    let mut alpha = fixture::tree(&[], true);
    let mut beta = fixture::tree(&[], true);
    for tree in [&mut alpha, &mut beta] {
        reference_with(tree, (0xd0_0065, "Remote Tasks"), 1, "Remote", &elsewhere);
    }
    reference(&mut alpha, (0xd0_0070, "Beta Tasks"), 2, "Beta");
    occurrence(&mut alpha, 0xd0_0071, "Alpha Missing", 2, 0x80fd);
    let host = Host::new(
        "attributed",
        vec![
            ("Root.fmp12", root),
            ("Alpha.fmp12", alpha),
            ("Beta.fmp12", beta),
        ],
    );
    let mut provider = host.open("Root.fmp12");
    provider.calc_schema().unwrap().unwrap();
    let warnings = provider.related_file_warnings().to_vec();
    let mut expected: Vec<String> = ["Alpha", "Beta"]
        .iter()
        .map(|file| {
            format!(
                "Data source “Remote” of “{file}” names no file OpenMaker opens: it opens a \
                 plain file name on the host “{file}” is on, or an fmnet path to a host and \
                 port, and does not follow folders, volumes or other forms. Its tables are \
                 unavailable."
            )
        })
        .collect();
    expected.extend(
        [
            "Occurrences “Old Tasks” and “Gone Tasks” of “Root” show a table that “Alpha” \
             does not have as the host sent it; their table is unavailable.",
            "Occurrence “Root Missing” of “Root” shows a table that “Beta” does not have as \
             the host sent it; the occurrence's table is unavailable.",
            "Occurrence “Alpha Missing” of “Alpha” shows a table that “Beta” does not have as \
             the host sent it; the occurrence's table is unavailable.",
            "A data source of “Root” used by “Lost Tasks” is not among the file's data \
             sources as the host sent them, so its tables are unavailable.",
            "A data source of “Root” used by “Bare Tasks” and “Bare Notes” has neither a name \
             nor a list of file paths, so its tables are unavailable.",
            "A data source of “Root” whose name OpenMaker cannot read, used by “Garbled \
             Tasks”, has no list of file paths, so its tables are unavailable.",
            "Data source “Chunked” of “Root” has a list of file paths OpenMaker cannot read, \
             so its tables are unavailable.",
            "Data source “Empty” of “Root” lists no file paths, so its tables are unavailable.",
            "Data source “Broken” of “Root” has a list of file paths OpenMaker cannot read, so \
             its tables are unavailable.",
        ]
        .map(str::to_owned),
    );
    for line in &expected {
        assert!(warnings.contains(line), "{line}\nin {warnings:?}");
    }
    assert!(
        warnings
            .iter()
            .all(|w| !w.contains("0x") && !w.contains("account can read") && !w.contains("“Gone”")),
        "{warnings:?}"
    );
    provider.close();
}

/// The limit of 128 counts Opens, not data sources: 129 data sources that
/// each list the opened file first, then a file of their own, all bind to
/// the opened file without an Open, and none is left out (#748's review).
#[test]
fn data_sources_naming_an_open_file_need_no_open_and_count_toward_no_limit() {
    let mut root = fixture::tree(&[], true);
    let unused: Vec<String> = (0..129).map(|n| format!("Unused {n}")).collect();
    for (n, file) in unused.iter().enumerate() {
        let n = n as u64;
        reference_with(
            &mut root,
            (0xd1_0000 + n, &format!("Aliased {n}")),
            n + 1,
            &format!("Aliased {n}"),
            &[("file", "", "Root"), ("file", "", file.as_str())],
        );
    }
    let host = Host::new("aliases", vec![("Root.fmp12", root)]);
    let mut provider = host.open("Root.fmp12");
    let schema = provider.calc_schema().unwrap().unwrap();
    for n in 0..129 {
        assert!(schema.table_of(0xd1_0000 + n).is_some(), "Aliased {n}");
    }
    assert!(
        provider.related_file_warnings().is_empty(),
        "{:?}",
        provider.related_file_warnings()
    );
    let (opens, logins) = host.captures.opens_and_logins();
    assert_eq!(opens.get("Root.fmp12"), Some(&1));
    assert_eq!(logins.get("Root.fmp12"), Some(&1));
    assert_eq!(opens.len(), 1, "{opens:?}");
    provider.close();
}

/// A related file that refused the account is sent it once per session
/// (#738): not again by the steps that open it (`open_peer`, as Open File,
/// Perform Script in another file and Go to Related Record do), by an
/// opened peer's own related files, or by a Reconnect. Opening the first
/// file again starts a new session, which tries it once more. A server that
/// counts failed Logins sees one per session.
#[test]
fn a_refusing_related_file_gets_one_login_per_session() {
    let host = Host::new("refusals", graph());
    // Denied's Opens and Logins, under any spelling.
    let denied = || {
        let (opens, logins) = host.captures.opens_and_logins();
        let count = |counts: &std::collections::BTreeMap<String, usize>| {
            counts
                .iter()
                .filter(|(name, _)| name.eq_ignore_ascii_case("Denied.fmp12"))
                .map(|(_, n)| *n)
                .sum::<usize>()
        };
        (count(&opens), count(&logins))
    };
    let unavailable = "Related file “Denied” (data source “Denied” of ";
    let connection = host.connection("Root.fmp12");
    let mut provider = host.provider();
    provider.open(&connection).unwrap();
    provider.calc_schema().unwrap();
    assert_eq!(denied(), (1, 1));
    // Root and its related files, Denied's refused Login included, on one
    // connection (#740).
    assert_eq!(host.handle.clients().len(), 1);
    // Two steps that open it, under either spelling: refused at once, with
    // the reason it had, without contacting the host.
    for name in ["Denied.fmp12", "DENIED.fmp12"] {
        match provider.open_peer(name) {
            Ok(_) => panic!("Denied opened"),
            Err(error) => assert_eq!(error.code, Some(212), "{error:?}"),
        }
    }
    // A peer that opens: Denied is among its related files too.
    let mut gamma = provider
        .open_peer("Gamma.fmp12")
        .unwrap_or_else(|e| panic!("{e:?}"));
    gamma.calc_schema().unwrap();
    let warnings = gamma.related_file_warnings();
    assert!(
        warnings.iter().any(|w| w.contains(unavailable)),
        "{warnings:?}"
    );
    gamma.close();
    assert_eq!(denied(), (1, 1));
    // A Reconnect is the same session.
    provider.reopen(&connection).unwrap();
    provider.calc_schema().unwrap();
    let warnings = provider.related_file_warnings();
    assert!(
        warnings.iter().any(|w| w.contains(unavailable)),
        "{warnings:?}"
    );
    assert_eq!(denied(), (1, 1));
    // Opening the first file again is a new session.
    provider.open(&connection).unwrap();
    provider.calc_schema().unwrap();
    assert_eq!(denied(), (2, 2));
    provider.close();
}

/// A related file that refused the account opens with another (#214). It
/// is listed with the occurrences that need it; each answer is one Login,
/// none for a pair it refused before; the file joins the schema after the
/// others, which keep their ids.
#[test]
fn a_refusing_related_file_opens_with_its_own_account() {
    let host = Host::new("sign-in", graph());
    let logins = || {
        host.captures
            .opens_and_logins()
            .1
            .get("Denied.fmp12")
            .copied()
            .unwrap_or(0)
    };
    let tables = |schema: &om_data::host::CalcSchema| {
        schema
            .tables()
            .map(|table| (table.id, table.name.clone()))
            .collect::<std::collections::BTreeSet<_>>()
    };
    let mut provider = host.provider();
    provider.open(&host.connection("Root.fmp12")).unwrap();
    let before = tables(&provider.calc_schema().unwrap().unwrap());
    let warnings = provider.related_file_warnings();
    assert!(
        warnings.iter().any(|w| w.contains("“Denied”")),
        "{warnings:?}"
    );
    // Root's occurrence of it is what needs it.
    assert_eq!(
        provider.related_sign_ins(),
        [om_data::RelatedSignIn {
            file: "Denied".into(),
            occurrences: [0xd1_0007].into(),
        }]
    );
    assert_eq!(logins(), 1);
    // The pair it refused stays refused, without a Login.
    let error = provider
        .sign_in_related("Denied", ACCOUNT, fixture::PASSWORD)
        .unwrap_err();
    assert_eq!(error.code, Some(212), "{error:?}");
    assert_eq!(logins(), 1);
    // A wrong password: one Login, and not sent again.
    for _ in 0..2 {
        let error = provider
            .sign_in_related("Denied", "Other account", "wrong")
            .unwrap_err();
        assert_eq!(error.code, Some(212), "{error:?}");
    }
    assert_eq!(logins(), 2);
    // Its own account opens it: Root's and Gamma's occurrences of it
    // resolve, and the other files' tables keep their ids.
    provider
        .sign_in_related("Denied", "Other account", fixture::PASSWORD)
        .unwrap();
    assert_eq!(logins(), 3);
    assert!(provider.related_sign_ins().is_empty());
    let schema = provider.calc_schema().unwrap().unwrap();
    assert!(schema.table_of(0xd1_0007).is_some());
    let after = tables(&schema);
    assert_eq!(after.len(), before.len() + 2, "{after:?}");
    assert!(before.is_subset(&after), "{before:?} {after:?}");
    let warnings = provider.related_file_warnings();
    assert!(
        warnings.iter().all(|w| !w.contains("“Denied”")),
        "{warnings:?}"
    );
    provider.close();
}

/// Files on two hosts (#976). Root, opened on this host, names `Related`
/// by a relative path (this host's), by an `fmnet` path to the other
/// host's port, and in a data source listing that path before a relative
/// one. The other host's files have a password of their own: the opened
/// file's account is offered there once, on a verified connection, and
/// refused, so the window is asked to sign in (`related_sign_ins`, the
/// file named by its `fmnet` path), and the data source listing it first
/// does not fall through to this host's `Related`. A refused sign-in is
/// the other host's own; the next opens the file there, and its relative
/// `Sub` on that host too, with the same account in the same session. The
/// two `Related`s are two files, and the other host's connection goes
/// when the file is closed.
#[test]
fn files_on_another_host_open_with_their_own_account() {
    let mut related_there = fixture::tree(&[], true);
    reference(&mut related_there, (0xd0_0080, "Sub Tasks"), 1, "Sub");
    fixture::put_account(&mut related_there, 2, THEIRS, None, 1);
    let mut sub = fixture::tree(&[], true);
    fixture::put_account(&mut sub, 2, THEIRS, None, 1);
    let other = Host::new(
        "other-host",
        vec![("Related.fmp12", related_there), ("Sub.fmp12", sub)],
    );
    let mut root = fixture::tree(&[], true);
    let elsewhere = format!("\u{E000}127.0.0.1:{}\u{E000}", other.port);
    reference_with(
        &mut root,
        (0xd0_0040, "Remote Tasks"),
        1,
        "Elsewhere",
        &[("fmnet", &elsewhere, "Related")],
    );
    reference_with(
        &mut root,
        (0xd0_0041, "First Tasks"),
        2,
        "First",
        &[("fmnet", &elsewhere, "Related"), ("file", "", "Related")],
    );
    reference(&mut root, (0xd0_0042, "Local Tasks"), 3, "Related");
    let host = Host::new(
        "this-host",
        vec![
            ("Root.fmp12", root),
            ("Related.fmp12", fixture::tree(&[], true)),
        ],
    );
    // The client trusts both hosts' certificates: it verifies the other
    // host's as the opened one's.
    let mut provider = host.provider_trusting(&other);
    provider.open(&host.connection("Root.fmp12")).unwrap();
    let schema = provider.calc_schema().unwrap().unwrap();
    assert!(schema.table_of(0xd0_0040).is_none());
    assert!(
        schema.table_of(0xd0_0041).is_none(),
        "no fall-through to this host's Related"
    );
    let local = schema.table_of(0xd0_0042).expect("this host's Related").id;
    let remote = format!("fmnet:/127.0.0.1:{}/Related", other.port);
    assert_eq!(
        provider.related_sign_ins(),
        [om_data::RelatedSignIn {
            file: remote.clone(),
            occurrences: [0xd0_0040, 0xd0_0041].into(),
        }]
    );
    let line = format!(
        "Related file “Related” (data source “Elsewhere” of “Root”): it is on another host \
         (127.0.0.1:{}), which did not admit the account this file was opened with; sign in to \
         open it.",
        other.port
    );
    assert!(
        provider.related_file_warnings().contains(&line),
        "{line}\nin {:?}",
        provider.related_file_warnings()
    );
    let (_, logins) = other.captures.opens_and_logins();
    assert_eq!(
        logins.get("Related.fmp12"),
        Some(&1),
        "one offer: {logins:?}"
    );
    assert_eq!(logins.get("Sub.fmp12"), None, "{logins:?}");
    // A wrong password is the other host's refusal.
    let error = provider
        .sign_in_related(&remote, ACCOUNT, "wrong")
        .unwrap_err();
    assert_eq!(error.code, Some(212), "{error:?}");
    provider.sign_in_related(&remote, ACCOUNT, THEIRS).unwrap();
    assert!(provider.related_sign_ins().is_empty());
    let schema = provider.calc_schema().unwrap().unwrap();
    let id = |occurrence| schema.table_of(occurrence).map(|table| table.id);
    let there = id(0xd0_0040).expect("the other host's Related");
    assert_eq!(id(0xd0_0041), Some(there));
    assert_eq!(id(0xd0_0042), Some(local));
    assert_ne!(there, local, "two files of one name");
    // Each names its own host and port in `Get ( FilePath )`: the other
    // port's file is not this host's file of the same name.
    let path = |occurrence| {
        let table = schema.table_of(occurrence)?.id;
        schema.table_context(table)?.file_path.clone()
    };
    assert_eq!(
        path(0xd0_0040),
        Some(format!("fmnet:/127.0.0.1:{}/Related.fmp12", other.port))
    );
    // This host's file keeps the path derived beside the opened file's.
    assert_eq!(path(0xd0_0042), None);
    // And which file each is: its host, port and name.
    let identity = |occurrence| {
        let table = schema.table_of(occurrence)?.id;
        schema.table_context(table)?.identity.clone()
    };
    use om_data::file_refs::FileIdentity;
    assert_eq!(
        identity(0xd0_0040),
        Some(FileIdentity::hosted("127.0.0.1", other.port, "Related"))
    );
    assert_eq!(
        identity(0xd0_0042),
        Some(FileIdentity::hosted("127.0.0.1", host.port, "Related"))
    );
    let (opens, logins) = other.captures.opens_and_logins();
    assert_eq!(opens.get("Related.fmp12"), Some(&3), "{opens:?}");
    assert_eq!(logins.get("Related.fmp12"), Some(&3), "{logins:?}");
    assert_eq!(opens.get("Sub.fmp12"), Some(&1), "{opens:?}");
    assert_eq!(logins.get("Sub.fmp12"), Some(&1), "{logins:?}");
    // The offer's, the wrong sign-in's, and the one `Related` and `Sub`
    // share.
    assert_eq!(other.captures.connections(), 3);
    // The account that host admitted stays after the sign-in: a script's
    // open of its file there is sent it, unasked, and holds the related
    // Sub's open of it there, with no Open or Login of its own (#1011).
    let sub = format!("fmnet:/127.0.0.1:{}/Sub", other.port);
    let mut there = provider.open_peer(&sub).unwrap();
    assert_eq!(there.server_port(), other.port);
    there.close();
    assert_eq!(
        other.captures.opens_and_logins(),
        (
            [("Related.fmp12".to_owned(), 3), ("Sub.fmp12".to_owned(), 1)].into(),
            [("Related.fmp12".to_owned(), 3), ("Sub.fmp12".to_owned(), 1)].into()
        )
    );
    assert_eq!(other.captures.connections(), 3, "no connection of its own");
    let (opens, logins) = host.captures.opens_and_logins();
    assert_eq!(opens.get("Related.fmp12"), Some(&1), "{opens:?}");
    assert_eq!(logins.get("Related.fmp12"), Some(&1), "{logins:?}");
    assert_eq!(opens.get("Sub.fmp12"), None, "Sub is the other host's");
    assert!(!other.handle.clients().is_empty());
    provider.close();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !other.handle.clients().is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(other.handle.clients().is_empty(), "closed with the file");
}

/// The other host's files admit the opened file's account: it is offered
/// there once, on a verified connection, and opens `Related` with no
/// sign-in, as FileMaker Pro's does (network-protocol.md 5.5). `Related`'s
/// relative `Sub` opens with the same account in the same session: one
/// connection to that host for both.
#[test]
fn files_on_another_host_open_with_the_files_account_when_it_admits_it() {
    let mut related_there = fixture::tree(&[], true);
    reference(&mut related_there, (0xd0_0080, "Sub Tasks"), 1, "Sub");
    let other = Host::new(
        "admitting-host",
        vec![
            ("Related.fmp12", related_there),
            ("Sub.fmp12", fixture::tree(&[], true)),
        ],
    );
    let mut root = fixture::tree(&[], true);
    let elsewhere = format!("\u{E000}127.0.0.1:{}\u{E000}", other.port);
    reference_with(
        &mut root,
        (0xd0_0040, "Remote Tasks"),
        1,
        "Elsewhere",
        &[("fmnet", &elsewhere, "Related")],
    );
    let host = Host::new("admitted-here", vec![("Root.fmp12", root)]);
    let mut provider = host.provider_trusting(&other);
    provider.open(&host.connection("Root.fmp12")).unwrap();
    let schema = provider.calc_schema().unwrap().unwrap();
    assert!(
        schema.table_of(0xd0_0040).is_some(),
        "the other host's Related"
    );
    assert!(provider.related_sign_ins().is_empty());
    let (opens, logins) = other.captures.opens_and_logins();
    for file in ["Related.fmp12", "Sub.fmp12"] {
        assert_eq!(opens.get(file), Some(&1), "{file} {opens:?}");
        assert_eq!(logins.get(file), Some(&1), "{file} {logins:?}");
    }
    assert_eq!(other.captures.connections(), 1, "Sub in Related's session");
    provider.close();
}

/// A host whose certificate the client does not trust is sent no Login:
/// the opened file's account would go there, and the connection is
/// verified first. The window is not asked to sign in either, since an
/// account typed for it would not get further.
#[test]
fn a_host_whose_certificate_is_not_trusted_is_sent_no_login() {
    let other = Host::new(
        "untrusted-host",
        vec![("Related.fmp12", fixture::tree(&[], true))],
    );
    let mut root = fixture::tree(&[], true);
    let elsewhere = format!("\u{E000}127.0.0.1:{}\u{E000}", other.port);
    reference_with(
        &mut root,
        (0xd0_0040, "Remote Tasks"),
        1,
        "Elsewhere",
        &[("fmnet", &elsewhere, "Related")],
    );
    let host = Host::new("untrusting-here", vec![("Root.fmp12", root)]);
    // Trusting this host's certificate only.
    let mut provider = host.provider();
    provider.open(&host.connection("Root.fmp12")).unwrap();
    let schema = provider.calc_schema().unwrap().unwrap();
    assert!(schema.table_of(0xd0_0040).is_none());
    assert!(provider.related_sign_ins().is_empty());
    assert!(
        provider
            .related_file_warnings()
            .iter()
            .any(|w| w.contains("could not connect to the host")),
        "{:?}",
        provider.related_file_warnings()
    );
    assert!(other.captures.opens_and_logins().1.is_empty());
    provider.close();
}

/// Signing in to one file on another host lets the next schema try that
/// host's other files waiting for an account, with no second sign-in; a
/// file the other host does not have, found at sign-in, sends the data
/// sources waiting at it on to their next path (here a file already open),
/// as when the schema is built, and no account is kept for it (#976, the
/// review of #977).
#[test]
fn signing_in_on_another_host_opens_its_other_files_and_passes_a_missing_one() {
    let theirs = || {
        let mut file = fixture::tree(&[], true);
        fixture::put_account(&mut file, 2, THEIRS, None, 1);
        file
    };
    let other = Host::new(
        "other-files",
        vec![("Related.fmp12", theirs()), ("Lookup.fmp12", theirs())],
    );
    let mut root = fixture::tree(&[], true);
    let there = format!("\u{E000}127.0.0.1:{}\u{E000}", other.port);
    reference_with(
        &mut root,
        (0xd0_0090, "Related Tasks"),
        1,
        "Remote",
        &[("fmnet", &there, "Related")],
    );
    reference_with(
        &mut root,
        (0xd0_0091, "Lookup Tasks"),
        2,
        "Lookup",
        &[("fmnet", &there, "Lookup")],
    );
    reference_with(
        &mut root,
        (0xd0_0092, "Fallback Tasks"),
        3,
        "Fallback",
        &[("fmnet", &there, "Missing"), ("file", "", "Here")],
    );
    // Here is open through a data source of its own: a later path naming
    // an open file needs no account (one first found when the schema is
    // built again would, and the opened file's is gone by then).
    reference(&mut root, (0xd0_0093, "Here Tasks"), 4, "Here");
    let host = Host::new(
        "this-files",
        vec![
            ("Root.fmp12", root),
            ("Here.fmp12", fixture::tree(&[], true)),
        ],
    );
    let mut provider = host.provider_trusting(&other);
    provider.open(&host.connection("Root.fmp12")).unwrap();
    let schema = provider.calc_schema().unwrap().unwrap();
    assert!((0xd0_0090..=0xd0_0092).all(|o| schema.table_of(o).is_none()));
    let name = |file: &str| format!("fmnet:/127.0.0.1:{}/{file}", other.port);
    let asked: Vec<String> = provider
        .related_sign_ins()
        .into_iter()
        .map(|s| s.file)
        .collect();
    assert_eq!(asked, [name("Related"), name("Lookup"), name("Missing")]);
    // The opened file's account was offered to Related only: refused there,
    // it goes to none of that host's other files.
    let (_, logins) = other.captures.opens_and_logins();
    assert_eq!(logins, [("Related.fmp12".to_owned(), 1)].into());
    // The other host does not have Missing: Fallback goes on to Here, and
    // the account typed for it is kept for nothing.
    provider
        .sign_in_related(&name("Missing"), ACCOUNT, THEIRS)
        .unwrap();
    let schema = provider.calc_schema().unwrap().unwrap();
    assert!(schema.table_of(0xd0_0092).is_some(), "Here's table");
    let asked: Vec<String> = provider
        .related_sign_ins()
        .into_iter()
        .map(|s| s.file)
        .collect();
    assert_eq!(asked, [name("Related"), name("Lookup")]);
    // One sign-in there opens both of its files.
    provider
        .sign_in_related(&name("Related"), ACCOUNT, THEIRS)
        .unwrap();
    let schema = provider.calc_schema().unwrap().unwrap();
    assert!(schema.table_of(0xd0_0090).is_some(), "Related's table");
    assert!(schema.table_of(0xd0_0091).is_some(), "Lookup's table");
    assert!(provider.related_sign_ins().is_empty());
    let (opens, logins) = other.captures.opens_and_logins();
    assert_eq!(opens.get("Missing.fmp12"), Some(&1), "{opens:?}");
    assert_eq!(logins.get("Missing.fmp12"), None, "{logins:?}");
    // Related: the offer and the sign-in.
    assert_eq!(opens.get("Related.fmp12"), Some(&2), "{opens:?}");
    assert_eq!(logins.get("Related.fmp12"), Some(&2), "{logins:?}");
    assert_eq!(opens.get("Lookup.fmp12"), Some(&1), "{opens:?}");
    assert_eq!(logins.get("Lookup.fmp12"), Some(&1), "{logins:?}");
    provider.close();
}

/// A related file on the opened host first reached when the schema is
/// built again opens with the account the file is open with (#978): here
/// a data source's next path, once the other host turned out not to have
/// its first, names a file nothing opened yet. The other host refused the
/// opened file's account (for `Guarded`, the one offer), so the first
/// schema waits at that path for an account typed for it; that host gets
/// the offer and the typed account, and nothing else.
#[test]
fn a_file_first_reached_on_a_rebuild_opens_with_the_files_account() {
    let mut guarded = fixture::tree(&[], true);
    fixture::put_account(&mut guarded, 2, THEIRS, None, 1);
    let other = Host::new("rebuild-other", vec![("Guarded.fmp12", guarded)]);
    let mut root = fixture::tree(&[], true);
    let there = format!("\u{E000}127.0.0.1:{}\u{E000}", other.port);
    reference_with(
        &mut root,
        (0xd0_009f, "Guarded Tasks"),
        1,
        "Guard",
        &[("fmnet", &there, "Guarded")],
    );
    reference_with(
        &mut root,
        (0xd0_00a0, "Fallback Tasks"),
        2,
        "Fallback",
        &[("fmnet", &there, "Missing"), ("file", "", "Here")],
    );
    let host = Host::new(
        "rebuild-here",
        vec![
            ("Root.fmp12", root),
            ("Here.fmp12", fixture::tree(&[], true)),
        ],
    );
    let mut provider = host.provider_trusting(&other);
    provider.open(&host.connection("Root.fmp12")).unwrap();
    let schema = provider.calc_schema().unwrap().unwrap();
    assert!(schema.table_of(0xd0_00a0).is_none());
    let missing = format!("fmnet:/127.0.0.1:{}/Missing", other.port);
    provider
        .sign_in_related(&missing, "Elsewhere", "typed for there")
        .unwrap();
    let schema = provider.calc_schema().unwrap().unwrap();
    assert!(schema.table_of(0xd0_00a0).is_some(), "Here's table");
    assert!(
        provider
            .related_file_warnings()
            .iter()
            .all(|w| !w.contains("open this file again")),
        "{:?}",
        provider.related_file_warnings()
    );
    let (opens, logins) = host.captures.opens_and_logins();
    assert_eq!(opens.get("Here.fmp12"), Some(&1), "{opens:?}");
    assert_eq!(logins.get("Here.fmp12"), Some(&1), "{logins:?}");
    let (opens, logins) = other.captures.opens_and_logins();
    assert_eq!(opens.get("Missing.fmp12"), Some(&1), "{opens:?}");
    assert_eq!(
        logins,
        [("Guarded.fmp12".to_owned(), 1)].into(),
        "the one offer"
    );
    provider.close();
}

/// An `fmnet` path naming the host and port the file was opened from names
/// a file on that host, stored as FileMaker Pro 26.0.3 stores one (U+E000
/// around the host and port, #759), and is tried in the paths' order: a
/// file the host does not have is passed on, and the next opens with one
/// Login. A path to the same host's text at another port is another host's
/// file (#976), and so is a path without a port, which names port 5003,
/// not this host's. The client trusts neither's certificate: neither is
/// sent a Login, the opened file's account included, and the data source
/// listing such a path first ends there.
#[test]
fn a_data_source_follows_fmnet_paths_to_the_opened_host() {
    let other = Host::new(
        "fmnet-other",
        vec![("Related.fmp12", fixture::tree(&[], true))],
    );
    let other_port = other.port;
    let host = Host::naming_its_port("fmnet-here", |port| {
        let at = |authority: &str| format!("\u{E000}{authority}\u{E000}");
        let (elsewhere, other_host, here, default) = (
            at(&format!("127.0.0.1:{other_port}")),
            at("elsewhere.invalid"),
            at(&format!("127.0.0.1:{port}")),
            at("127.0.0.1"),
        );
        let mut root = fixture::tree(&[], true);
        reference_with(
            &mut root,
            (0xd0_0070, "Here Tasks"),
            1,
            "Here",
            &[("fmnet", &here, "Missing"), ("fmnet", &here, "Related")],
        );
        reference_with(
            &mut root,
            (0xd0_0072, "Elsewhere Tasks"),
            3,
            "Elsewhere",
            &[
                ("fmnet", &elsewhere, "Related"),
                ("fmnet", &other_host, "Related"),
                ("fmnet", &here, "Related"),
            ],
        );
        reference_with(
            &mut root,
            (0xd0_0071, "Default Port Tasks"),
            2,
            "Default",
            &[("fmnet", &default, "Related")],
        );
        vec![
            ("Root.fmp12", root),
            ("Related.fmp12", fixture::tree(&[], true)),
        ]
    });
    assert_ne!(host.port, 5003);
    let mut provider = host.open("Root.fmp12");
    let schema = provider.calc_schema().unwrap().unwrap();
    assert!(schema.table_of(0xd0_0070).is_some(), "Related's table");
    assert!(schema.table_of(0xd0_0071).is_none());
    assert!(schema.table_of(0xd0_0072).is_none(), "no fall-through");
    let warnings = provider.related_file_warnings();
    for source in ["Default", "Elsewhere"] {
        let line = format!("(data source “{source}” of “Root”): OpenMaker could not connect");
        assert!(
            warnings.iter().any(|w| w.contains(&line)),
            "{line}\nin {warnings:?}"
        );
    }
    let (opens, logins) = host.captures.opens_and_logins();
    assert_eq!(opens.get("Missing.fmp12"), Some(&1));
    assert_eq!(logins.get("Missing.fmp12"), None);
    assert_eq!(opens.get("Related.fmp12"), Some(&1));
    assert_eq!(logins.get("Related.fmp12"), Some(&1));
    assert!(other.handle.clients().is_empty());
    assert!(
        other.captures.transactions().is_empty(),
        "the other port was never contacted"
    );
    provider.close();
}

/// A related file whose host could not be reached is not failed for the
/// session (#740): on demand, the read that wanted it finds its occurrence
/// waiting, with the warning, and the answer says when to ask again; a
/// read within the retry interval does not try the host again, even once
/// it is up; once the interval passed, the next read opens the file there,
/// with the opened file's account offered once (#1010), no Login having
/// gone to the host while it was down.
#[test]
fn a_related_file_whose_host_was_down_opens_when_next_read() {
    use om_data::host::CalcNeed;
    let ca = related::Ca::new();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut root = fixture::tree(&[], true);
    let there = format!("\u{E000}127.0.0.1:{port}\u{E000}");
    reference_with(
        &mut root,
        (0xd0_0040, "Remote Tasks"),
        1,
        "Elsewhere",
        &[("fmnet", &there, "Related")],
    );
    let host = Host::new("retry-here", vec![("Root.fmp12", root)]);
    let hour = std::time::Duration::from_secs(3600);
    let mut provider = host
        .provider_trusting_ca(&ca)
        .with_related_on_demand(true)
        .with_related_retry_after(hour);
    provider.open(&host.connection("Root.fmp12")).unwrap();
    provider.calc_schema().unwrap();
    let wanted = [CalcNeed::Occurrence {
        occurrence: 0xd0_0040,
    }];
    let answer = provider.calc_data(&wanted).unwrap();
    assert!(
        answer.retry_after.is_some_and(|after| after > hour / 2),
        "{:?}",
        answer.retry_after
    );
    let schema = provider.calc_schema().unwrap().unwrap();
    assert!(schema.table_of(0xd0_0040).is_none());
    assert!(schema.is_pending(0xd0_0040), "waiting, not failed");
    let warned = |provider: &om_provider::NativeProvider| {
        provider
            .related_file_warnings()
            .iter()
            .filter(|w| w.contains("could not connect") && w.contains("tried again"))
            .count()
    };
    assert_eq!(
        warned(&provider),
        1,
        "{:?}",
        provider.related_file_warnings()
    );
    assert!(
        provider.related_sign_ins().is_empty(),
        "no account asked for"
    );

    let other = Host::serving_at(
        "retry-other",
        vec![("Related.fmp12", fixture::tree(&[], true))],
        port,
        &ca,
    )
    .expect("the port is free again");
    let _ = provider.calc_data(&wanted);
    assert_eq!(
        other.captures.connections(),
        0,
        "not tried within the interval"
    );
    assert_eq!(warned(&provider), 1, "still told");
    provider.set_related_retry_after(std::time::Duration::ZERO);
    let answer = provider.calc_data(&wanted).unwrap();
    assert_eq!(answer.retry_after, None);
    let schema = provider.calc_schema().unwrap().unwrap();
    assert!(
        schema.table_of(0xd0_0040).is_some(),
        "opened once its host was up"
    );
    assert_eq!(warned(&provider), 0);
    let (opens, logins) = other.captures.opens_and_logins();
    assert_eq!(opens.get("Related.fmp12"), Some(&1), "{opens:?}");
    assert_eq!(logins.get("Related.fmp12"), Some(&1), "{logins:?}");
    provider.close();
}

/// At most 128 related files are opened, and the user is told once, also
/// when the limit is reached in the middle of a batch whose files name
/// more.
#[test]
fn at_most_128_related_files_are_opened() {
    let mut root = fixture::tree(&[], true);
    // The first batch contains only File 0. Its next 131 references make
    // the 128th attempt land inside a batch, after one failed Open.
    reference(&mut root, (0xd0_0040, "First"), 1, "File 0");
    let mut first = fixture::tree(&[], true);
    reference(&mut first, (0xd1_0000, "Missing"), 1000, "Missing");
    let mut files = Vec::new();
    for n in 1..=130u64 {
        let name = format!("File {n}");
        reference(
            &mut first,
            (0xd1_0000 + n, &format!("Related {n}")),
            n,
            &name,
        );
        // Each also names a file found only after the limit was reached.
        let mut tree = fixture::tree(&[], true);
        reference(&mut tree, (0xd1_0000, "Later"), 1, &format!("Later {n}"));
        files.push((format!("{name}.fmp12"), tree));
    }
    let mut hosted: Vec<(&str, om_hbam::OwnedNode)> =
        vec![("Root.fmp12", root), ("File 0.fmp12", first)];
    hosted.extend(
        files
            .iter()
            .map(|(name, tree)| (name.as_str(), tree.clone())),
    );
    let host = Host::new("limit", hosted);
    let mut provider = host.open("Root.fmp12");
    let schema = provider.calc_schema().unwrap().unwrap();
    assert_eq!(
        schema.tables().count(),
        2 * 128,
        "Root and 127 successful related Opens, two tables each"
    );
    let warnings = provider.related_file_warnings();
    assert_eq!(
        warnings
            .iter()
            .filter(|w| w.contains("more than 128 related files"))
            .count(),
        1,
        "{warnings:?}"
    );
    let (opens, _) = host.captures.opens_and_logins();
    assert_eq!(
        opens.len(),
        129,
        "Root plus 128 attempts (127 files and Missing): {opens:?}"
    );
    assert!(opens.contains_key("File 126.fmp12"));
    assert!(!opens.contains_key("File 127.fmp12"));
    assert!(opens.keys().all(|file| !file.starts_with("Later")));
    provider.close();
}

/// Files that could not be opened count toward the 128 too: each was
/// tried. The window gets a bounded list of what went wrong.
#[test]
fn failed_opens_count_toward_the_file_limit() {
    let mut root = fixture::tree(&[], true);
    for n in 1..=132u64 {
        let name = format!("Missing {n}");
        reference(&mut root, (0xd1_0000 + n, &name), n, &name);
    }
    let host = Host::new("failed-limit", vec![("Root.fmp12", root)]);
    let mut provider = host.open("Root.fmp12");
    provider.calc_schema().unwrap();
    let (opens, _) = host.captures.opens_and_logins();
    assert_eq!(opens.len(), 129, "Root and 128 tried files: {opens:?}");
    assert!(!opens.contains_key("Missing 129.fmp12"));
    let warnings = provider.related_file_warnings();
    assert!(warnings.len() <= 20, "{}", warnings.len());
    assert!(
        warnings[0].contains("more than 128 related files"),
        "{warnings:?}"
    );
    assert!(
        warnings.last().unwrap().contains("more problems"),
        "{warnings:?}"
    );
    provider.close();
}

/// `Root.fmp12.fmp12` and `Root.fmp12` are two hosted files: a reference to
/// `Root` from the first names the second, not the opened file itself.
#[test]
fn a_name_ending_in_the_extension_is_another_file() {
    const OTHER: u64 = 0xd0_0040;
    let mut root = fixture::tree(&[], true);
    reference(&mut root, (OTHER, "Other Tasks"), 1, "Root");
    let host = Host::new(
        "double-extension",
        vec![
            ("Root.fmp12.fmp12", root),
            ("Root.fmp12", fixture::tree(&[], true)),
        ],
    );
    let mut provider = host.open("Root.fmp12.fmp12");
    let schema = provider.calc_schema().unwrap().unwrap();
    let table = schema.table_of(OTHER).expect("Other Tasks is resolved").id;
    assert_ne!(table, fixture::TABLE, "not the opened file's own Tasks");
    let (opens, logins) = host.captures.opens_and_logins();
    assert_eq!(opens.get("Root.fmp12"), Some(&1), "{opens:?}");
    assert_eq!(logins.get("Root.fmp12"), Some(&1), "{logins:?}");
    provider.close();
}

/// A Close the window queues while the related files are being opened
/// stops the opening: the files whose Open the slow host still answers do
/// not log in, no further file is opened, the credentials are gone and
/// the related connections closed.
#[test]
fn closing_while_related_files_open_stops_opening_them() {
    use std::time::Duration;
    let mut root = fixture::tree(&[], true);
    let mut files = Vec::new();
    for n in 1..=12u64 {
        let name = format!("Slow {n}");
        reference(&mut root, (0xd1_0000 + n, &name), n, &name);
        files.push((format!("{name}.fmp12"), fixture::tree(&[], true)));
    }
    let mut hosted: Vec<(&str, om_hbam::OwnedNode)> = vec![("Root.fmp12", root)];
    hosted.extend(
        files
            .iter()
            .map(|(name, tree)| (name.as_str(), tree.clone())),
    );
    let host = Host::configured("closing", hosted, |server| {
        files.iter().fold(server, |server, (name, _)| {
            server.delaying_opens(name, Duration::from_millis(400))
        })
    });
    let mut provider = host.open("Root.fmp12");
    let closes = provider.close_requests().unwrap();
    let opening = std::thread::spawn(move || {
        let result = provider.calc_schema().map(drop);
        (provider, result)
    });
    // The first files' Opens are on their way when the window closes: the
    // host has read one (on Root's connection, #740).
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !host
        .captures
        .opens_and_logins()
        .0
        .keys()
        .any(|file| file.starts_with("Slow"))
    {
        assert!(
            std::time::Instant::now() < deadline,
            "no related file was opened"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    closes.queued();
    let (mut provider, result) = opening.join().unwrap();
    assert!(result.unwrap_err().message.contains("closing"));
    let (opens, logins) = host.captures.opens_and_logins();
    let related = opens.keys().filter(|file| file.starts_with("Slow")).count();
    assert!(related <= 4, "only the first batch was opened: {opens:?}");
    assert!(
        logins.keys().all(|file| !file.starts_with("Slow")),
        "no related file logs in once the window closes: {logins:?}"
    );
    // Root's connection is left with Root alone.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while host
        .handle
        .clients()
        .iter()
        .any(|client| client.files.len() > 1)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "related files stay open"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    // The credentials went with it: built again, the schema opens nothing.
    closes.handled();
    provider.calc_schema().unwrap();
    assert_eq!(host.captures.opens_and_logins().0, opens);
    assert!(provider
        .related_file_warnings()
        .iter()
        .all(|w| w.contains("it was not opened; open this file again")));
    provider.close();
}

/// The occurrence of the related file in [`remote_root`], and its
/// unstored calculation Remote Name = `Remote Tasks::Name`.
const REMOTE: u64 = 0xd0_0040;
const REMOTE_NAME: u64 = 60;

/// A root whose Tasks has Remote Name = `Remote Tasks::Name` over a ×
/// relationship to the Tasks of file `file` at `paths`: both its records'
/// Remote Name is that file's first Name, "Write tests" (#1125).
fn remote_root(paths: &[(&str, &str, &str)]) -> om_hbam::OwnedNode {
    use fixture::{NAME, OCCURRENCE};
    // The field option word's "do not store" bit (schema-path.md 5.5).
    const UNSTORED: u32 = 0x2_0000;
    let mut root = fixture::tree(&[], true);
    reference_with(&mut root, (REMOTE, "Remote Tasks"), 1, "Related", paths);
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, NAME), 6, (REMOTE, NAME));
    fixture::add_field(
        &mut root,
        TABLE,
        REMOTE_NAME,
        "Remote Name",
        2,
        1,
        0,
        UNSTORED,
        0,
    );
    let mut names = fixture::names();
    names.add_occurrence(REMOTE, "Remote Tasks", TABLE);
    names.add_field(TABLE, REMOTE_NAME, "Remote Name");
    fixture::add_formula(
        &mut root,
        &names,
        (TABLE, OCCURRENCE),
        REMOTE_NAME,
        5,
        "Remote Tasks::Name",
    );
    root
}

fn tasks_layout(provider: &mut om_provider::NativeProvider) -> u32 {
    provider
        .layouts()
        .unwrap()
        .into_iter()
        .find(|layout| layout.name == "Tasks")
        .unwrap()
        .id
}

/// Find Remote Name = "Write tests" on `layout`: the found count, or the
/// error's code.
fn find_remote(
    provider: &mut om_provider::NativeProvider,
    layout: u32,
) -> Result<u64, Option<i32>> {
    use om_data::found::{FindKind, FindRequest, FoundSetChange};
    let mut request = FindRequest::default();
    request.set("Remote Name", "Write tests");
    provider
        .change_found_set(
            layout,
            FoundSetChange::Find {
                requests: vec![request],
                kind: FindKind::Replace,
            },
        )
        .map(|found| found.found)
        .map_err(|e| e.code)
}

/// A find on an unstored calculation reading a related file not open yet
/// opens the file and finds what the same find finds once it is open
/// (#1125): both records. Before, it read the file as missing, compared
/// `?` and found none (401).
#[test]
fn a_find_on_a_calculation_reading_a_related_file_not_open_yet_opens_it() {
    use om_data::host::CalcNeed;
    let host = Host::new(
        "find-pending",
        vec![
            ("Root.fmp12", remote_root(&[("file", "", "Related")])),
            ("Related.fmp12", fixture::tree(&[], true)),
        ],
    );
    let mut provider = host.provider().with_related_on_demand(true);
    provider.open(&host.connection("Root.fmp12")).unwrap();
    let schema = provider.calc_schema().unwrap().unwrap();
    assert!(schema.is_pending(REMOTE), "not opened before the find");
    let tasks = tasks_layout(&mut provider);
    let first = find_remote(&mut provider, tasks);
    let schema = provider.calc_schema().unwrap().unwrap();
    assert!(schema.table_of(REMOTE).is_some(), "the find opened it");
    let _ = provider
        .calc_data(&[CalcNeed::Occurrence { occurrence: REMOTE }])
        .unwrap();
    let after = find_remote(&mut provider, tasks);
    assert_eq!(after, Ok(2), "the reference: both records");
    assert_eq!(first, after, "the first find");
    let (opens, _) = host.captures.opens_and_logins();
    assert_eq!(opens.get("Related.fmp12"), Some(&1), "{opens:?}");
    provider.close();
}

/// A record page's unstored calculation reading a related file not open
/// yet shows the related value, the file opened for it, not `?` (#1125).
#[test]
fn a_page_calculation_reading_a_related_file_not_open_yet_opens_it() {
    let host = Host::new(
        "page-pending",
        vec![
            ("Root.fmp12", remote_root(&[("file", "", "Related")])),
            ("Related.fmp12", fixture::tree(&[], true)),
        ],
    );
    let mut provider = host.provider().with_related_on_demand(true);
    provider.open(&host.connection("Root.fmp12")).unwrap();
    assert!(provider.calc_schema().unwrap().unwrap().is_pending(REMOTE));
    let tasks = tasks_layout(&mut provider);
    let page = provider.records(tasks, 0, 10).unwrap();
    let column = page
        .fields
        .iter()
        .position(|f| f.name == "Remote Name")
        .expect("the calculation is on the page");
    let values: Vec<Option<&str>> = page
        .records
        .iter()
        .map(|r| r.values[column].as_deref())
        .collect();
    assert_eq!(values, [Some("Write tests"), Some("Write tests")]);
    provider.close();
}

/// A find whose calculation reads a related file that could not be opened
/// (its host not reached, not tried again within the interval) fails with
/// 3 naming the file, rather than comparing `?` and leaving records out as
/// if they did not match (#1125).
#[test]
fn a_find_on_a_calculation_reading_a_file_that_does_not_open_fails() {
    // The related file's host: a listener this test keeps, closing every
    // connection at once, so no TLS handshake completes and no other server
    // can take its port meanwhile.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            drop(stream);
        }
    });
    let there = format!("\u{E000}127.0.0.1:{port}\u{E000}");
    let host = Host::new(
        "find-unreached",
        vec![("Root.fmp12", remote_root(&[("fmnet", &there, "Related")]))],
    );
    let mut provider = host
        .provider()
        .with_related_on_demand(true)
        .with_related_retry_after(std::time::Duration::from_secs(3600));
    provider.open(&host.connection("Root.fmp12")).unwrap();
    let tasks = tasks_layout(&mut provider);
    assert_eq!(find_remote(&mut provider, tasks), Err(Some(3)));
    assert!(
        provider.calc_schema().unwrap().unwrap().is_pending(REMOTE),
        "still waiting for its host"
    );
    provider.close();
}

/// The items of a value list of an unstored calculation reading a related
/// file not open yet are what they are once the file is open (#1125): its
/// one value, "Write tests". Before, the calculation's `?` failed the list
/// before its file was asked for.
#[test]
fn a_value_list_of_a_calculation_reading_a_related_file_not_open_yet_opens_it() {
    let items = remote_list_items("list-pending", (fixture::OCCURRENCE, REMOTE_NAME));
    assert_eq!(items, ["Write tests"]);
}

/// A value list of a related file's own field, `Remote Tasks::Name`, whose
/// file is not open yet, lists that field's values once the file is open:
/// "Ship" and "Write tests", its two records'.
/// The list reader asks for the occurrence's file (stage A of #772's lazy
/// field catalogs), where it read the occurrence as a missing table (106),
/// so nothing asked for the file.
#[test]
fn a_value_list_of_a_related_files_field_not_open_yet_opens_it() {
    let items = remote_list_items("list-remote", (REMOTE, fixture::NAME));
    assert_eq!(items, ["Ship", "Write tests"]);
}

/// The items of value list 5, the values of `list` (an occurrence and
/// field), shown by an edit box of Tasks::Name on Root's layout 3, the
/// related file opening on demand.
fn remote_list_items(tag: &str, list: (u64, u64)) -> Vec<String> {
    use fixture::{NAME, OCCURRENCE};
    use om_layout::encode;
    const LIST: u64 = 5;
    let mut root = remote_root(&[("file", "", "Related")]);
    // Value list 5: the values of `list` (file-metadata.md 6.2).
    let key = |id: u64| {
        let segment = uint_segment(id);
        [vec![segment.len() as u8 + 1, segment.len() as u8], segment].concat()
    };
    let (list_occurrence, list_field) = list;
    let mut list = vec![1, 0, 0];
    list.extend(key(list_occurrence));
    list.extend(key(list_field));
    list.extend(1u16.to_be_bytes());
    list.extend([0, 0, 0, 0, 0, 0, 0]);
    list.extend(om_scsu::encode_counted(" "));
    root.put(
        &[&[0x21], &[5], &uint_segment(LIST), &[0x10]],
        om_scripts::text_node(&[], "Remote names"),
    );
    root.put(
        &[&[0x21], &[5], &uint_segment(LIST), &[2]],
        om_hbam::OwnedNode::leaf(Vec::new(), list),
    );
    // Layout 3 "Names": an edit box of Tasks::Name with that list.
    fixture::index_layouts(&mut root);
    let catalog = root.at(&[&[4]]).cloned().unwrap();
    let mut member = om_data::layout_catalog::plan(
        &catalog,
        &om_data::layout_catalog::NewLayout {
            name: "Names".into(),
            occurrence: OCCURRENCE,
            theme: vec![1],
            width: 1000.0,
        },
    )
    .unwrap()
    .member;
    member.segment = uint_segment(3);
    let frame = om_layout::Rect {
        top: 20.0,
        left: 20.0,
        bottom: 40.0,
        right: 130.0,
        ..Default::default()
    };
    let planned = om_data::layout_placement::plan(
        &member,
        &om_data::layout_placement::Placement {
            occurrence: OCCURRENCE,
            field: NAME,
            frame: frame.clone(),
            label: None,
        },
    )
    .unwrap();
    let (mut member, object) = (planned.member, planned.field_object);
    let serial = |id| om_data::layout_placement::serial_value(id).unwrap();
    let mut spec = Vec::new();
    encode::uint(1, serial(OCCURRENCE), &mut spec);
    encode::uint(2, serial(NAME), &mut spec);
    encode::uint(3, 1, &mut spec);
    let mut field = Vec::new();
    encode::uint(1, encode::EDIT_BOX_FLAGS, &mut field);
    encode::bytes(2, &spec, &mut field);
    encode::uint(3, 1, &mut field);
    encode::uint(4, 0, &mut field);
    encode::uint(5, 0, &mut field);
    encode::uint(6, serial(LIST), &mut field);
    encode::uint(7, 1, &mut field);
    encode::uint(8, 1, &mut field);
    let mut control = Vec::new();
    encode::uint(1, u64::from(encode::FIELD), &mut control);
    encode::uint(2, object, &mut control);
    encode::uint(3, encode::ANCHOR_LEFT_TOP, &mut control);
    encode::bytes(4, &encode::rect(&frame), &mut control);
    encode::bytes(
        5,
        &encode::default_style(encode::EDIT_BOX_STYLE),
        &mut control,
    );
    encode::bytes(8, &field, &mut control);
    let store = member
        .child(&[0x07])
        .and_then(|n| n.value.clone())
        .unwrap_or_default();
    let without = encode::without_objects(&store, &[object]).unwrap();
    let store = encode::append_to_store(&without, &[object], &[control]);
    member.set_child(om_hbam::OwnedNode::leaf(vec![0x07], store));
    root.put(&[&[4], &[5], &member.segment.clone()], member);
    fixture::index_layouts(&mut root);
    fixture::index_layout_definitions(&mut root);
    let host = Host::new(
        tag,
        vec![
            ("Root.fmp12", root),
            ("Related.fmp12", fixture::tree(&[], true)),
        ],
    );
    let mut provider = host.provider().with_related_on_demand(true);
    provider.open(&host.connection("Root.fmp12")).unwrap();
    assert!(provider.calc_schema().unwrap().unwrap().is_pending(REMOTE));
    let listed = provider
        .value_list_choices(om_data::choice::ChoiceTarget {
            layout: 3,
            object,
            record: 1,
        })
        .unwrap_or_else(|e| panic!("{e:?}"));
    let items = listed
        .choices
        .items
        .iter()
        .map(|item| item.value.clone())
        .collect();
    provider.close();
    items
}

/// Root's Tasks with Copied, a text field `entry` gives its rule, related
/// to the related file's Tasks through Name = Name (a write's rules read
/// related records through `=` only, #187), on demand: a record named
/// "Write tests" created after the related file was opened
/// (`open_first`) or before anything opened it, and its Copied, or the
/// create's error.
fn create_reading_related(
    tag: &str,
    open_first: bool,
    entry: impl Fn(&mut om_hbam::OwnedNode),
) -> Result<Option<String>, (Option<i32>, String)> {
    use fixture::{NAME, OCCURRENCE};
    use om_data::host::CalcNeed;
    let mut root = fixture::tree(&[], true);
    reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, NAME), 0, (REMOTE, NAME));
    entry(&mut root);
    let host = Host::new(
        tag,
        vec![
            ("Root.fmp12", root),
            ("Related.fmp12", fixture::tree(&[], true)),
        ],
    );
    let mut provider = host.provider().with_related_on_demand(true);
    provider.open(&host.connection("Root.fmp12")).unwrap();
    if open_first {
        let _ = provider
            .calc_data(&[CalcNeed::Occurrence { occurrence: REMOTE }])
            .unwrap();
    }
    assert_eq!(
        provider.calc_schema().unwrap().unwrap().is_pending(REMOTE),
        !open_first
    );
    let tasks = tasks_layout(&mut provider);
    let changes = [("Name".to_owned(), Some("Write tests".to_owned()))].into();
    let result = provider.create_record(tasks, changes).map(|record| {
        let page = provider.records(tasks, 0, 10).unwrap();
        let column = page.fields.iter().position(|f| f.name == "Copied").unwrap();
        page.records
            .iter()
            .find(|r| r.id == record.id)
            .and_then(|r| r.values[column].clone())
    });
    provider.close();
    result.map_err(|e| (e.code, e.message))
}

/// Copied's field id in [`create_reading_related`].
const COPIED: u64 = 61;

/// A write's auto-entered calculation reading a related file not open yet
/// enters what it enters once the file is open (#1139): Copied,
/// auto-entered by `Remote Tasks::Name`, is "Write tests" either way.
/// Before, with the file open the create failed reading it ("The related
/// file holding this table is not open."), and before it was open the
/// field was refused as reading a missing occurrence.
#[test]
fn an_auto_entered_calculation_reading_a_related_file_not_open_yet_opens_it() {
    use fixture::OCCURRENCE;
    // The field option word's auto-entered calculated value (schema-path.md 5.5).
    const CALCULATED_VALUE: u32 = 0x8;
    let entry = |root: &mut om_hbam::OwnedNode| {
        fixture::add_field(root, TABLE, COPIED, "Copied", 1, 1, 0, CALCULATED_VALUE, 0);
        let mut names = fixture::names();
        names.add_occurrence(REMOTE, "Remote Tasks", TABLE);
        names.add_field(TABLE, COPIED, "Copied");
        fixture::add_formula(
            root,
            &names,
            (TABLE, OCCURRENCE),
            COPIED,
            5,
            "Remote Tasks::Name",
        );
    };
    let after = create_reading_related("entry-open", true, entry);
    assert_eq!(after, Ok(Some("Write tests".to_owned())), "the reference");
    let before = create_reading_related("entry-pending", false, entry);
    assert_eq!(before, after, "before the file was open");
}

/// A write's looked-up value from a related file not open yet copies what
/// it copies once the file is open (#1139): Copied looks up
/// `Remote Tasks::Name` through Name = Name.
#[test]
fn a_looked_up_value_from_a_related_file_not_open_yet_opens_it() {
    use fixture::{NAME, OCCURRENCE};
    use om_data::field_options::{Lookup, NoMatch};
    // The field option word's looked-up value, and its definition's member
    // (schema-path.md 5.5, 5.13).
    const LOOKED_UP: u32 = 0x400;
    let entry = |root: &mut om_hbam::OwnedNode| {
        fixture::add_field(root, TABLE, COPIED, "Copied", 1, 1, 0, LOOKED_UP, 0);
        let lookup = Lookup {
            from: OCCURRENCE,
            to: REMOTE,
            copied: NAME,
            copy_empty: false,
            no_match: NoMatch::DoNotCopy,
        };
        root.put(
            &[
                &uint_segment(TABLE),
                &[3],
                &[5],
                &uint_segment(COPIED),
                &[0x08],
            ],
            om_hbam::OwnedNode::leaf(Vec::new(), lookup.encode().unwrap()),
        );
    };
    let after = create_reading_related("lookup-open", true, entry);
    assert_eq!(after, Ok(Some("Write tests".to_owned())), "the reference");
    let before = create_reading_related("lookup-pending", false, entry);
    assert_eq!(before, after, "before the file was open");
}

/// The window's record capabilities, asked before a layout is drawn, open
/// no related file only a write's rules read (#1139): with Copied
/// auto-entered by `Remote Tasks::Name` and the related file not open,
/// New Record and edits are offered, Copied among the editable fields, and
/// the file stays closed. The first create opens it and enters the related
/// value.
#[test]
fn record_capabilities_defer_the_files_only_a_writes_rules_read() {
    use fixture::{NAME, OCCURRENCE};
    const CALCULATED_VALUE: u32 = 0x8;
    let mut root = fixture::tree(&[], true);
    reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, NAME), 0, (REMOTE, NAME));
    fixture::add_field(
        &mut root,
        TABLE,
        COPIED,
        "Copied",
        1,
        1,
        0,
        CALCULATED_VALUE,
        0,
    );
    const GLOBAL_TEXT: u64 = 92;
    const GLOBAL_NUMBER: u64 = 93;
    const GLOBAL_STORAGE: u32 = 0x1_0000;
    const NUMERIC_ONLY: u32 = 0x1000;
    fixture::add_field(
        &mut root,
        TABLE,
        GLOBAL_TEXT,
        "Global text",
        1,
        1,
        0,
        GLOBAL_STORAGE,
        0,
    );
    fixture::add_field(
        &mut root,
        TABLE,
        GLOBAL_NUMBER,
        "Global number",
        1,
        2,
        0,
        GLOBAL_STORAGE,
        NUMERIC_ONLY,
    );
    let mut names = fixture::names();
    names.add_occurrence(REMOTE, "Remote Tasks", TABLE);
    names.add_field(TABLE, COPIED, "Copied");
    fixture::add_formula(
        &mut root,
        &names,
        (TABLE, OCCURRENCE),
        COPIED,
        5,
        "Remote Tasks::Name",
    );
    let host = Host::new(
        "capabilities-deferred",
        vec![
            ("Root.fmp12", root),
            ("Related.fmp12", fixture::tree(&[], true)),
        ],
    );
    let mut provider = host.provider().with_related_on_demand(true);
    provider.open(&host.connection("Root.fmp12")).unwrap();
    let tasks = tasks_layout(&mut provider);
    let capabilities = provider.record_capabilities(tasks).unwrap();
    assert!(
        capabilities.create && capabilities.update,
        "{capabilities:?}"
    );
    let editable = capabilities.editable_fields.clone().unwrap();
    assert!(
        editable.contains("Copied") && editable.contains("Name"),
        "{editable:?}"
    );
    assert!(editable.contains("Global text"), "{editable:?}");
    assert!(editable.contains("Global number"), "{editable:?}");
    assert!(provider.calc_schema().unwrap().unwrap().is_pending(REMOTE));
    assert_eq!(
        host.captures.opens_and_logins().0.get("Related.fmp12"),
        None
    );
    let changes = [("Name".to_owned(), Some("Write tests".to_owned()))].into();
    let record = provider.create_record(tasks, changes).unwrap();
    let page = provider.records(tasks, 0, 10).unwrap();
    let column = page.fields.iter().position(|f| f.name == "Copied").unwrap();
    let copied = page
        .records
        .iter()
        .find(|r| r.id == record.id)
        .and_then(|r| r.values[column].clone());
    assert_eq!(copied.as_deref(), Some("Write tests"));
    assert_eq!(
        host.captures.opens_and_logins().0.get("Related.fmp12"),
        Some(&1)
    );
    provider.close();
}

/// A write's plan opens only the related files its rules read (#1139):
/// root's unstored Remote Name and a global calculation of the same formula
/// read the related file, but a write computes neither, so planning a
/// write (Set Field's own check of a value, which reads no page) opens
/// nothing.
#[test]
fn a_writes_plan_opens_no_file_only_an_unstored_calculation_reads() {
    use fixture::OCCURRENCE;
    // A calculation with global storage, option `0x10000` alone
    // (schema-path.md 5.10).
    const GLOBAL_NAME: u64 = 62;
    let mut root = remote_root(&[("file", "", "Related")]);
    fixture::add_field(
        &mut root,
        TABLE,
        GLOBAL_NAME,
        "Global Name",
        2,
        1,
        0,
        0x1_0000,
        0,
    );
    let mut names = fixture::names();
    names.add_occurrence(REMOTE, "Remote Tasks", TABLE);
    names.add_field(TABLE, GLOBAL_NAME, "Global Name");
    fixture::add_formula(
        &mut root,
        &names,
        (TABLE, OCCURRENCE),
        GLOBAL_NAME,
        5,
        "Remote Tasks::Name",
    );
    let host = Host::new(
        "write-unstored",
        vec![
            ("Root.fmp12", root),
            ("Related.fmp12", fixture::tree(&[], true)),
        ],
    );
    let mut provider = host.provider().with_related_on_demand(true);
    provider.open(&host.connection("Root.fmp12")).unwrap();
    // A value Set Field enters is checked by the write's rules
    // (`check_entry`, which reads no page). The planner refuses the global
    // calculation's options (#187), so what is checked is that planning
    // opened nothing, not its answer.
    let _ = provider.check_entry(TABLE, fixture::NAME, "New");
    assert!(provider.calc_schema().unwrap().unwrap().is_pending(REMOTE));
    let (opens, _) = host.captures.opens_and_logins();
    assert_eq!(opens.get("Related.fmp12"), None, "{opens:?}");
    provider.close();
}

/// A write whose capabilities were deferred still fails explicitly when the
/// related file its rules read does not open (#1139): Copied reads Remote
/// Tasks, whose host is never reached. New Record is offered (the plan
/// waits for the file), and the create is refused, storing nothing.
#[test]
fn a_deferred_write_whose_file_does_not_open_is_refused() {
    use fixture::{NAME, OCCURRENCE};
    const CALCULATED_VALUE: u32 = 0x8;
    // The related file's host: kept, closing every connection at once.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            drop(stream);
        }
    });
    let there = format!("\u{E000}127.0.0.1:{port}\u{E000}");
    let mut root = fixture::tree(&[], true);
    reference_with(
        &mut root,
        (REMOTE, "Remote Tasks"),
        1,
        "Related",
        &[("fmnet", &there, "Related")],
    );
    fixture::add_relationship(&mut root, 9, (OCCURRENCE, NAME), 0, (REMOTE, NAME));
    fixture::add_field(
        &mut root,
        TABLE,
        COPIED,
        "Copied",
        1,
        1,
        0,
        CALCULATED_VALUE,
        0,
    );
    let mut names = fixture::names();
    names.add_occurrence(REMOTE, "Remote Tasks", TABLE);
    names.add_field(TABLE, COPIED, "Copied");
    fixture::add_formula(
        &mut root,
        &names,
        (TABLE, OCCURRENCE),
        COPIED,
        5,
        "Remote Tasks::Name",
    );
    let host = Host::new("deferred-unreached", vec![("Root.fmp12", root)]);
    let mut provider = host
        .provider()
        .with_related_on_demand(true)
        .with_related_retry_after(std::time::Duration::from_secs(3600));
    provider.open(&host.connection("Root.fmp12")).unwrap();
    let tasks = tasks_layout(&mut provider);
    let capabilities = provider.record_capabilities(tasks).unwrap();
    assert!(capabilities.create, "{capabilities:?}");
    let before = provider.records(tasks, 0, 10).unwrap().records.len();
    let changes = [("Name".to_owned(), Some("Write tests".to_owned()))].into();
    let error = provider.create_record(tasks, changes).unwrap_err();
    assert!(error.message.contains("Copied"), "{error}");
    assert_eq!(
        provider.records(tasks, 0, 10).unwrap().records.len(),
        before
    );
    assert!(provider.calc_schema().unwrap().unwrap().is_pending(REMOTE));
    provider.close();
}

/// A related table's capabilities defer as the layout's own table's do
/// (#1139): a layout of Tasks shows Notes::Body, one relationship away
/// (Name = Task), and Notes has Copied, auto-entered by `Remote Tasks::Name`
/// (Task = Name), a file not open yet. Notes' fields are offered for
/// editing, the file staying closed; its write would open it first.
#[test]
fn a_related_tables_capabilities_defer_the_files_only_its_writes_read() {
    use fixture::{BODY, NAME, NOTES, NOTES_OCCURRENCE, OCCURRENCE, TASK};
    const CALCULATED_VALUE: u32 = 0x8;
    let mut root = fixture::tree(&[], true);
    reference(&mut root, (REMOTE, "Remote Tasks"), 1, "Related");
    fixture::add_field(&mut root, NOTES, TASK, "Task", 1, 1, 0, 0, 0);
    fixture::add_relationship(
        &mut root,
        1,
        (OCCURRENCE, NAME),
        0,
        (NOTES_OCCURRENCE, TASK),
    );
    fixture::add_relationship(&mut root, 10, (NOTES_OCCURRENCE, TASK), 0, (REMOTE, NAME));
    fixture::add_field(
        &mut root,
        NOTES,
        COPIED,
        "Copied",
        1,
        1,
        0,
        CALCULATED_VALUE,
        0,
    );
    let mut names = fixture::names();
    names.add_occurrence(REMOTE, "Remote Tasks", TABLE);
    names.add_field(NOTES, TASK, "Task");
    names.add_field(NOTES, COPIED, "Copied");
    fixture::add_formula(
        &mut root,
        &names,
        (NOTES, NOTES_OCCURRENCE),
        COPIED,
        5,
        "Remote Tasks::Name",
    );
    // Layout 3 "Names", of Tasks, showing Notes::Body.
    fixture::index_layouts(&mut root);
    let catalog = root.at(&[&[4]]).cloned().unwrap();
    let mut member = om_data::layout_catalog::plan(
        &catalog,
        &om_data::layout_catalog::NewLayout {
            name: "Names".into(),
            occurrence: OCCURRENCE,
            theme: vec![1],
            width: 1000.0,
        },
    )
    .unwrap()
    .member;
    member.segment = uint_segment(3);
    let frame = om_layout::Rect {
        top: 20.0,
        left: 20.0,
        bottom: 40.0,
        right: 130.0,
        ..Default::default()
    };
    let member = place_related(&member, &frame, (NOTES_OCCURRENCE, BODY));
    root.put(&[&[4], &[5], &member.segment.clone()], member);
    fixture::index_layouts(&mut root);
    fixture::index_layout_definitions(&mut root);
    let host = Host::new(
        "related-deferred",
        vec![
            ("Root.fmp12", root),
            ("Related.fmp12", fixture::tree(&[], true)),
        ],
    );
    let mut provider = host.provider().with_related_on_demand(true);
    provider.open(&host.connection("Root.fmp12")).unwrap();
    let capabilities = provider.record_capabilities(3).unwrap();
    assert!(capabilities.create, "{capabilities:?}");
    let notes = capabilities.related.get(&NOTES_OCCURRENCE);
    assert!(
        notes.is_some_and(|fields| fields.contains(&BODY)),
        "{capabilities:?}"
    );
    assert!(provider.calc_schema().unwrap().unwrap().is_pending(REMOTE));
    assert_eq!(
        host.captures.opens_and_logins().0.get("Related.fmp12"),
        None
    );
    provider.close();
}

/// `member` with an edit box of field `field` of `occurrence` at `frame`,
/// one of another table: placed as one of the layout's own table
/// (Tasks::Name, all `layout_placement` places), its object then given the
/// related field and its field reference index entry moved to it, as
/// FileMaker Pro stores a related field (layout-model.md 5.3).
fn place_related(
    member: &om_hbam::OwnedNode,
    frame: &om_layout::Rect,
    (occurrence, field): (u64, u64),
) -> om_hbam::OwnedNode {
    use fixture::{NAME, OCCURRENCE};
    use om_layout::encode;
    let planned = om_data::layout_placement::plan(
        member,
        &om_data::layout_placement::Placement {
            occurrence: OCCURRENCE,
            field: NAME,
            frame: frame.clone(),
            label: None,
        },
    )
    .unwrap();
    let (mut member, object) = (planned.member, planned.field_object);
    let serial = |id| om_data::layout_placement::serial_value(id).unwrap();
    let mut spec = Vec::new();
    encode::uint(1, serial(occurrence), &mut spec);
    encode::uint(2, serial(field), &mut spec);
    encode::uint(3, 1, &mut spec);
    let mut body = Vec::new();
    encode::uint(1, encode::EDIT_BOX_FLAGS, &mut body);
    encode::bytes(2, &spec, &mut body);
    encode::uint(3, 1, &mut body);
    encode::uint(4, 0, &mut body);
    encode::uint(5, 0, &mut body);
    encode::uint(7, 1, &mut body);
    encode::uint(8, 1, &mut body);
    let mut control = Vec::new();
    encode::uint(1, u64::from(encode::FIELD), &mut control);
    encode::uint(2, object, &mut control);
    encode::uint(3, encode::ANCHOR_LEFT_TOP, &mut control);
    encode::bytes(4, &encode::rect(frame), &mut control);
    encode::bytes(
        5,
        &encode::default_style(encode::EDIT_BOX_STYLE),
        &mut control,
    );
    encode::bytes(8, &body, &mut control);
    let store = member
        .child(&[0x07])
        .and_then(|n| n.value.clone())
        .unwrap_or_default();
    let without = encode::without_objects(&store, &[object]).unwrap();
    let store = encode::append_to_store(&without, &[object], &[control]);
    member.set_child(om_hbam::OwnedNode::leaf(vec![0x07], store));
    let key = om_hbam::serial::encode(object);
    member.remove(&[
        &[0x01],
        &uint_segment(OCCURRENCE),
        &uint_segment(NAME),
        &key,
    ]);
    member.put(
        &[
            &[0x01],
            &uint_segment(occurrence),
            &uint_segment(field),
            &key,
        ],
        om_hbam::OwnedNode::leaf(Vec::new(), Vec::new()),
    );
    member
}
