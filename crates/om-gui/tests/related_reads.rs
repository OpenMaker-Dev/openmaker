//! What the window asks a host for, per record and per List view page, on
//! a solution of two files (#804): Orders, whose layout shows records of
//! Catalog's large table through relationships, a portal and value lists.
//! Both files are built in memory (`om-server/tests/fixtures/tasks.rs`) and
//! hosted by an in-process OpenMaker Server on loopback that records the
//! connection; the test counts the requests in the recording.
mod common;
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
mod hosted;
mod two_files;

use std::path::Path;
use std::time::Instant;

use fixture::{ACCOUNT, LAYOUT, OCCURRENCE, PASSWORD};
use two_files::*;

/// Host Orders (its first layout showing `shown`) and Catalog, open
/// Orders, and count the requests of one Next Record and of one List view
/// page.
fn measure(shown: &[Shown], catalog: &Path) -> String {
    let dir = tempdir("related-reads");
    let orders_file = fixture::TempFile::from_tree("related-reads-orders", &orders(shown));
    let hosted = hosted::host_files_recorded(
        "related-reads",
        &[
            (Path::new(&orders_file.0), "Orders.fmp12"),
            (catalog, "Catalog.fmp12"),
        ],
        (ACCOUNT, PASSWORD),
        |server| server,
        Some(dir.clone()),
    );
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the window", |app| app.state().is_open());
    while let Some(error) = app.state().error().map(str::to_owned) {
        println!("alert: {error}");
        common::click(&mut app, "OK");
    }
    quiet(&mut app, &dir);
    let (opened, _) = recorded(&dir);
    let mut out = format!("{shown:?}: open {} requests", opened.len());
    // One Next Record.
    let (before, received_before) = recorded(&dir);
    let t = Instant::now();
    common::click(&mut app, "Next");
    quiet(&mut app, &dir);
    let (after, received_after) = recorded(&dir);
    let step = &after[before.len()..];
    out += &format!(
        "\n  next record: {} requests, {} B received, {:.2}s: {:?}",
        step.len(),
        received_after - received_before,
        t.elapsed().as_secs_f64() - 0.5,
        tally(step)
    );
    // List view: the first page.
    let (before, received_before) = recorded(&dir);
    let t = Instant::now();
    shot(&mut app, &format!("{shown:?}-form"));
    {
        use egui_kittest::kittest::{NodeT as _, Queryable};
        let node = app.get_by_label("List View");
        println!(
            "List View: disabled {}, role {:?}",
            node.accesskit_node().is_disabled(),
            node.accesskit_node().role()
        );
    }
    common::click(&mut app, "List View");
    app.run_steps(3);
    println!("after the click: view {:?}", app.state().view());
    quiet(&mut app, &dir);
    let (after, received_after) = recorded(&dir);
    let page = &after[before.len()..];
    out += &format!(
        "\n  list view: view {:?}, {} requests, {} B received, {:.2}s: {:?}",
        app.state().view(),
        page.len(),
        received_after - received_before,
        t.elapsed().as_secs_f64() - 0.5,
        tally(page)
    );
    shot(&mut app, &format!("{shown:?}-list"));
    out
}

#[test]
#[ignore = "measurement for #804; prints request counts"]
fn requests_per_record_and_page_by_what_the_layout_shows() {
    let catalog = fixture::TempFile::from_tree("related-reads-catalog", &catalog());
    let mut report = Vec::new();
    for shown in [
        vec![],
        vec![Shown::RelatedField],
        vec![Shown::Portal],
        vec![Shown::LabelMenu],
        vec![Shown::NameList],
        vec![Shown::Plug],
    ] {
        let line = measure(&shown, Path::new(&catalog.0));
        println!("{line}");
        report.push(line);
    }
    println!("{}", report.join("\n"));
}

/// The ids the relationship to `BY_COUNT` relates to order `record`, as a
/// window asks for a portal's rows.
fn portal_ids(provider: &mut om_provider::NativeProvider, record: u64) -> std::sync::Arc<[u64]> {
    related_ids(provider, record, BY_COUNT)
}

/// The ids the relationship to occurrence `to` relates to order `record`.
fn related_ids(
    provider: &mut om_provider::NativeProvider,
    record: u64,
    to: u64,
) -> std::sync::Arc<[u64]> {
    use om_data::host::CalcNeed;
    use om_data::DataProvider;
    let need = CalcNeed::RelatedIds {
        from: OCCURRENCE,
        record,
        to,
    };
    let data = provider.calc_data(&[need]).expect("calculation data");
    data.related_ids
        .get(&(OCCURRENCE, record, to))
        .or_else(|| data.related.get(&(OCCURRENCE, record, to)))
        .cloned()
        .expect("the related ids")
}

/// Download Lists of records of table `8001` of file `file` in `requests`
/// (both files' tables have that id).
fn record_downloads(requests: &[Request], file: u32) -> usize {
    requests
        .iter()
        .filter(|r| r.op == "op92" && r.key == "8001/05" && r.file == file)
        .count()
}

/// The file most of `requests`' record downloads name: Catalog's, on a
/// scan of it.
fn scanned_file(requests: &[Request]) -> u32 {
    let mut by = std::collections::BTreeMap::<u32, usize>::new();
    for r in requests
        .iter()
        .filter(|r| r.op == "op92" && r.key == "8001/05")
    {
        *by.entry(r.file).or_default() += 1;
    }
    by.into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(file, _)| file)
        .unwrap_or(0)
}

/// A relationship the host's index cannot answer (Catalog::Count has no
/// index) is matched by scanning Catalog. The scan reads its 20,000
/// records once for the session; the next orders match against what it
/// kept (#804). Without that, every order read them all again (42 Download
/// Lists each).
#[test]
fn a_scanned_table_is_read_once_per_session_not_per_record() {
    let dir = tempdir("related-reads-once");
    let catalog = fixture::TempFile::from_tree("related-reads-once-catalog", &catalog());
    let orders =
        fixture::TempFile::from_tree("related-reads-once-orders", &orders(&[Shown::Portal]));
    let hosted = hosted::host_files_recorded(
        "related-reads-once",
        &[
            (Path::new(&orders.0), "Orders.fmp12"),
            (Path::new(&catalog.0), "Catalog.fmp12"),
        ],
        (ACCOUNT, PASSWORD),
        |server| server,
        Some(dir.clone()),
    );
    let mut provider = hosted.opened();
    // Order 1 has Count 3: Catalog's records 1 and 3, 13, ... 19993.
    assert_eq!(portal_ids(&mut provider, 1).len(), 2001);
    let (first, _) = recorded(&dir);
    let catalog_file = scanned_file(&first);
    assert!(
        record_downloads(&first, catalog_file) >= 40,
        "the first order scans Catalog"
    );
    for record in 2..=4 {
        // Orders 2, 3 and 4 have Counts 5, 3 and 4.
        let ids = portal_ids(&mut provider, record);
        assert!(ids.len() >= 2000, "order {record}: {}", ids.len());
    }
    let (all, _) = recorded(&dir);
    let later = &all[first.len()..];
    assert_eq!(
        record_downloads(later, catalog_file),
        0,
        "{:?}",
        tally(later)
    );
    // One HostTimeStamp per read takes the notifications they wait for.
    assert!(later.len() <= 9, "{:?}", tally(later));
    // Refresh Window keeps the scan unless it flushes cached join results.
    use om_data::DataProvider;
    provider.refresh_window(false, false).unwrap();
    let start = recorded(&dir).0.len();
    portal_ids(&mut provider, 5);
    assert_eq!(
        record_downloads(&recorded(&dir).0[start..], catalog_file),
        0
    );
    provider.refresh_window(true, false).unwrap();
    let start = recorded(&dir).0.len();
    portal_ids(&mut provider, 6);
    assert!(record_downloads(&recorded(&dir).0[start..], catalog_file) >= 40);
}

/// Read order 2's related set twice, the second time from the scan the
/// first kept; let `change` give Catalog's record 3 order 2's Count, 5,
/// from another connection; and check that the next read has it. The scan
/// is dropped on the change's notification, which a HostTimeStamp brings
/// before a kept scan is used (#804). The second read leaves order 2 and
/// the globals in the page, so the read after the change needs no download
/// that would bring the notification along.
fn kept_scan_sees_a_change(
    window: &mut om_provider::NativeProvider,
    change: impl FnOnce(&mut om_provider::NativeProvider),
) {
    // Order 2 has Count 5; Catalog's record 3 has Count 3.
    let before = portal_ids(window, 2);
    assert_eq!(portal_ids(window, 2), before);
    assert_eq!(before.len(), 2001);
    assert!(!before.contains(&3));
    change(window);
    let after = portal_ids(window, 2);
    assert!(after.contains(&3), "{} ids", after.len());
    assert_eq!(after.len(), 2002);
}

/// Give Catalog's record 3 Count 5 on `catalog`, a connection of its own.
fn change_catalog(catalog: &mut dyn om_data::DataProvider) {
    catalog
        .update_record(
            LAYOUT as u32,
            3,
            [("Count".to_owned(), Some("5".to_owned()))].into(),
        )
        .expect("change Catalog's record 3");
}

/// Orders (showing the portal) and Catalog hosted on loopback; `configure`
/// as in [`hosted::host_files_recorded`].
fn hosted_pair(
    name: &str,
    configure: impl FnOnce(om_server::Server) -> om_server::Server,
) -> hosted::Hosted {
    let catalog = fixture::TempFile::from_tree(&format!("{name}-catalog"), &catalog());
    let orders = fixture::TempFile::from_tree(&format!("{name}-orders"), &orders(&[Shown::Portal]));
    hosted::host_files_recorded(
        name,
        &[
            (Path::new(&orders.0), "Orders.fmp12"),
            (Path::new(&catalog.0), "Catalog.fmp12"),
        ],
        (ACCOUNT, PASSWORD),
        configure,
        None,
    )
}

/// A second connection to the host changes a match value in Catalog.
#[test]
fn a_kept_scan_is_dropped_when_another_client_changes_the_table() {
    use om_data::DataProvider;
    let hosted = hosted_pair("related-reads-fresh", |server| server);
    kept_scan_sees_a_change(&mut hosted.opened(), |_| {
        let mut other = hosted.provider();
        other
            .open(&om_data::Connection {
                file: "Catalog.fmp12".into(),
                ..hosted.connection.clone()
            })
            .expect("open Catalog");
        change_catalog(&mut other);
    });
}

/// Files on this computer: the window reads Catalog on a connection of its
/// own, not the one Orders is open on, and asks that one too.
#[test]
fn a_kept_scan_is_dropped_when_a_local_related_file_changes() {
    use om_data::DataProvider;
    let dir = tempdir("related-reads-local");
    for (name, tree) in [("Orders", orders(&[Shown::Portal])), ("Catalog", catalog())] {
        let file = fixture::TempFile::from_tree(&format!("related-reads-local-{name}"), &tree);
        std::fs::copy(&file.0, dir.join(format!("{name}.fmp12"))).unwrap();
    }
    let client = rcgen::generate_simple_self_signed(vec!["OpenMaker".into()]).unwrap();
    let mut window = om_provider::NativeProvider::new(om_provider::NativeConfig {
        client_cert_pem: client.cert.pem().into_bytes(),
        client_key_pem: client.key_pair.serialize_pem().into_bytes(),
        additional_ca_pem: Vec::new(),
        callback_host: "127.0.0.1".into(),
        machine: om_protocol::MachineInfo::default(),
    });
    let opened = window
        .open_local(&om_data::LocalFile {
            path: dir.join("Orders.fmp12"),
            credentials: Some((ACCOUNT.to_owned(), PASSWORD.to_owned().into())),
        })
        .expect("open Orders");
    assert!(matches!(opened, om_data::LocalOpen::Opened { .. }));
    kept_scan_sees_a_change(&mut window, |window| {
        let mut other = window.open_peer("Catalog").expect("open Catalog");
        change_catalog(&mut *other);
        other.close();
    });
    window.close();
    let _ = std::fs::remove_dir_all(dir);
}

/// What the provider's working set keeps without a scan is checked the same
/// way before it is used again (#1067): Catalog's record list, which the
/// cartesian relationship to `ALL` reads, loses a record another client
/// deletes, although the read after the delete needs nothing else from the
/// host.
#[test]
fn a_kept_record_list_loses_a_record_another_client_deletes() {
    use om_data::DataProvider;
    let hosted = hosted_pair("related-reads-kept-list", |server| server);
    let mut window = hosted.opened();
    let before = related_ids(&mut window, 1, ALL);
    assert_eq!(before.len(), CATALOG_RECORDS as usize);
    assert!(before.contains(&3));
    assert_eq!(related_ids(&mut window, 1, ALL), before);
    let mut other = hosted.provider();
    other
        .open(&om_data::Connection {
            file: "Catalog.fmp12".into(),
            ..hosted.connection.clone()
        })
        .expect("open Catalog");
    other
        .delete_record(LAYOUT as u32, 3)
        .expect("delete Catalog's record 3");
    drop(other);
    let after = related_ids(&mut window, 1, ALL);
    assert!(!after.contains(&3), "{} ids", after.len());
    assert_eq!(after.len(), before.len() - 1);
}

/// A HostTimeStamp that fails may have left notifications waiting: the
/// window drops what it kept rather than use it.
#[test]
fn a_kept_scan_is_dropped_when_the_check_fails() {
    use om_data::DataProvider;
    let mut failing = None;
    let hosted = hosted_pair("related-reads-failing", |server| {
        let (server, flag) = server.failing_timestamps();
        failing = Some(flag);
        server
    });
    let failing = failing.expect("the server's flag");
    kept_scan_sees_a_change(&mut hosted.opened(), |_| {
        let mut other = hosted.provider();
        other
            .open(&om_data::Connection {
                file: "Catalog.fmp12".into(),
                ..hosted.connection.clone()
            })
            .expect("open Catalog");
        change_catalog(&mut other);
        failing.store(true, std::sync::atomic::Ordering::Relaxed);
    });
}

/// A summary its own hide condition hides is not computed (#1023). While
/// every order is found, Related Total (the Total of `Sum ( Catalog
/// All::Count )`) is hidden by `Get ( FoundCount ) > 29`: the window reads
/// no record of Catalog, which computing it would read in full. Omitting
/// one order shows it, computed then over the 29 orders left.
#[test]
fn a_hidden_summary_is_computed_only_once_it_shows() {
    use egui_kittest::kittest::{By, Queryable};
    let dir = tempdir("related-reads-hidden");
    let catalog = fixture::TempFile::from_tree("related-reads-hidden-catalog", &catalog());
    let orders = fixture::TempFile::from_tree(
        "related-reads-hidden-orders",
        &orders(&[Shown::HiddenTotal]),
    );
    let hosted = hosted::host_files_recorded(
        "related-reads-hidden",
        &[
            (Path::new(&orders.0), "Orders.fmp12"),
            (Path::new(&catalog.0), "Catalog.fmp12"),
        ],
        (ACCOUNT, PASSWORD),
        |server| server,
        Some(dir.clone()),
    );
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the window", |app| app.state().is_open());
    quiet(&mut app, &dir);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    let (hidden, _) = recorded(&dir);
    // Catalog's Count values: 3 and 5, then each record's id modulo 10.
    let per_order: u64 = 3 + 5 + (3..=CATALOG_RECORDS).map(|id| id % 10).sum::<u64>();
    let total = |orders: u64| (per_order * orders).to_string();
    let all_orders = total(ORDERS_RECORDS);
    assert!(app.query_all(By::new().value(&all_orders)).next().is_none());
    shot(&mut app, "hidden-total");

    common::open_menu(&mut app, "Records");
    common::click(&mut app, "Omit Record");
    let shown = total(ORDERS_RECORDS - 1);
    common::wait_until(&mut app, "the total, once shown", |a| {
        a.query_all(By::new().value(&shown)).next().is_some()
    });
    shot(&mut app, "shown-total");
    let (all, _) = recorded(&dir);
    let after = &all[hidden.len()..];
    let catalog_file = scanned_file(after);
    assert!(
        record_downloads(after, catalog_file) > 0,
        "{:?}",
        tally(after)
    );
    let while_hidden: Vec<_> = hidden
        .iter()
        .filter(|r| {
            r.file == catalog_file && (r.op == "op86" || (r.op == "op92" && r.key == "8001/05"))
        })
        .cloned()
        .collect();
    assert!(while_hidden.is_empty(), "{:?}", tally(&while_hidden));
}

/// Record pages keep what the layout's calculations read (#1042). Orders
/// shows Catalog All::Name (Catalog's record 1, through a cartesian
/// relationship) and Related Total, the found set's total of every order's
/// `Sum ( Catalog All::Count )`, which reads all of Catalog. Reading the
/// next page of orders and going back reads nothing of Catalog again; a
/// name another client gives Catalog's record 1 moves the provider's
/// calculation generation on at the next page read, whose reply brings the
/// host's notification, and shows in the window.
#[test]
fn page_reads_keep_calculation_inputs_until_the_host_changes_them() {
    use egui_kittest::kittest::{By, Queryable};
    use om_data::DataProvider;
    let dir = tempdir("related-reads-pages");
    let catalog = fixture::TempFile::from_tree("related-reads-pages-catalog", &catalog());
    let orders = fixture::TempFile::from_tree(
        "related-reads-pages-orders",
        &orders(&[Shown::RelatedField, Shown::Total]),
    );
    let hosted = hosted::host_files_recorded(
        "related-reads-pages",
        &[
            (Path::new(&orders.0), "Orders.fmp12"),
            (Path::new(&catalog.0), "Catalog.fmp12"),
        ],
        (ACCOUNT, PASSWORD),
        |server| server,
        Some(dir.clone()),
    );
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the window", |app| app.state().is_open());
    quiet(&mut app, &dir);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    let (opened, _) = recorded(&dir);
    let catalog_file = scanned_file(&opened);
    // The window's one connection until the other client below.
    let go_to = |app: &mut common::App, record: &str| {
        let (before, received_before) = recorded(&dir);
        common::open_menu(app, "Records");
        common::click(app, "Go to Record");
        common::click(app, record);
        quiet(app, &dir);
        let (after, received_after) = recorded(&dir);
        let step = &after[before.len()..];
        println!(
            "{record}: {} requests, {} B received: {:?}",
            step.len(),
            received_after - received_before,
            tally(step)
        );
        step.iter()
            .filter(|r| r.file == catalog_file && matches!(r.op.as_str(), "op86" | "op90" | "op92"))
            .count()
    };
    assert_eq!(
        go_to(&mut app, "Last"),
        0,
        "the next page read Catalog again"
    );
    assert_eq!(
        go_to(&mut app, "First"),
        0,
        "the first page read Catalog again"
    );

    // What the window keeps rests on the provider's generation, which a
    // page read moves on only when its reply brings a change: another
    // window's provider, with Catalog open too, sees it.
    let mut reader = hosted.opened();
    let all = om_data::host::CalcNeed::Occurrence { occurrence: ALL };
    reader.calc_data(&[all]).expect("open Catalog");
    // As the window reads its pages: no calculated field computed here.
    reader
        .records_with_summary(LAYOUT as u32, 0, 25, &[])
        .expect("a page");
    let kept = reader.calc_generation();
    reader
        .records_with_summary(LAYOUT as u32, 0, 25, &[])
        .expect("the page again");
    assert_eq!(reader.calc_generation(), kept);

    let mut other = hosted.provider();
    other
        .open(&om_data::Connection {
            file: "Catalog.fmp12".into(),
            ..hosted.connection.clone()
        })
        .expect("open Catalog");
    other
        .update_record(
            LAYOUT as u32,
            1,
            [("Name".to_owned(), Some("Renamed in Catalog".to_owned()))].into(),
        )
        .expect("rename Catalog's record 1");
    drop(other);
    reader
        .records_with_summary(LAYOUT as u32, 0, 25, &[])
        .expect("the page after the rename");
    assert_ne!(reader.calc_generation(), kept);
    // A reverted transaction leaves nothing read while it held records.
    let held = reader.calc_generation();
    reader
        .begin_transaction(Default::default())
        .expect("a transaction");
    reader.revert_transaction().expect("its revert");
    assert_ne!(reader.calc_generation(), held);
    // The Notes layout's page is read from the host, and its reply brings
    // the notification. Back on Orders every Catalog input is in the
    // window's cache: only that page's notice of the change refreshes it.
    common::select_layout(&mut app, "Notes");
    quiet(&mut app, &dir);
    common::select_layout(&mut app, "Orders");
    common::wait_until(&mut app, "the new name", |a| {
        a.query_all(By::new().value("Renamed in Catalog"))
            .next()
            .is_some()
    });
}

/// What calculations read stays in the provider's working set while the
/// window's found set is large and its table has many occurrences (#1067).
/// The page read stores the one found set under every occurrence of the
/// layout's table; counted once per occurrence, 20,000 orders under 14 of
/// them passed the working set's id budget, so every page read discarded
/// it and the next batch read Catalog's record list again. The last page
/// of orders now reads nothing of Catalog; a name another client gives
/// Catalog's record 1 still shows after the next page read.
#[test]
fn a_large_found_set_under_many_occurrences_keeps_the_working_set() {
    use egui_kittest::kittest::{By, Queryable};
    use om_data::DataProvider;
    let dir = tempdir("related-reads-occurrences");
    let catalog = fixture::TempFile::from_tree("related-reads-occurrences-catalog", &catalog());
    let mut orders = orders(&[Shown::RelatedField]);
    fixture::add_tasks(&mut orders, ORDERS_RECORDS + 1..=CATALOG_RECORDS);
    for n in 0..13 {
        fixture::add_occurrence(
            &mut orders,
            0xd0_0050 + n,
            &format!("Orders {n}"),
            fixture::TABLE,
        );
    }
    let orders = fixture::TempFile::from_tree("related-reads-occurrences-orders", &orders);
    let hosted = hosted::host_files_recorded(
        "related-reads-occurrences",
        &[
            (Path::new(&orders.0), "Orders.fmp12"),
            (Path::new(&catalog.0), "Catalog.fmp12"),
        ],
        (ACCOUNT, PASSWORD),
        |server| server,
        Some(dir.clone()),
    );
    let mut app = common::harness(Box::new(hosted.provider()));
    hosted.fill_form(&mut app);
    common::click(&mut app, "Open");
    common::wait_until(&mut app, "the window", |app| app.state().is_open());
    quiet(&mut app, &dir);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    // Catalog's file id: the file of the record list Catalog All reads.
    let (opened, _) = recorded(&dir);
    let catalog_file = opened
        .iter()
        .filter(|r| r.op == "op90" && r.key == "8001/0d")
        .map(|r| r.file)
        .next_back()
        .expect("Catalog's record list");
    let (before, received_before) = recorded(&dir);
    common::open_menu(&mut app, "Records");
    common::click(&mut app, "Go to Record");
    common::click(&mut app, "Last");
    quiet(&mut app, &dir);
    let (after, received_after) = recorded(&dir);
    let step = &after[before.len()..];
    println!(
        "last page: {} requests, {} B received: {:?}",
        step.len(),
        received_after - received_before,
        tally(step)
    );
    let catalog_reads: Vec<_> = step
        .iter()
        .filter(|r| r.file == catalog_file && matches!(r.op.as_str(), "op86" | "op90" | "op92"))
        .cloned()
        .collect();
    assert!(catalog_reads.is_empty(), "{:?}", tally(&catalog_reads));

    let mut other = hosted.provider();
    other
        .open(&om_data::Connection {
            file: "Catalog.fmp12".into(),
            ..hosted.connection.clone()
        })
        .expect("open Catalog");
    other
        .update_record(
            LAYOUT as u32,
            1,
            [("Name".to_owned(), Some("Renamed in Catalog".to_owned()))].into(),
        )
        .expect("rename Catalog's record 1");
    drop(other);
    common::select_layout(&mut app, "Notes");
    quiet(&mut app, &dir);
    common::select_layout(&mut app, "Orders");
    common::wait_until(&mut app, "the new name", |a| {
        a.query_all(By::new().value("Renamed in Catalog"))
            .next()
            .is_some()
    });
}
