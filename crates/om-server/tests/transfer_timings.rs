//! Timings of the review's C4 and C5, for release builds (ignored by
//! default):
//!
//! ```sh
//! cargo test --release -p om-server --test transfer_timings -- --ignored --nocapture --test-threads 1
//! ```
//!
//! - A 20,000-row import (Add, auto-enter on) in a server script, while a
//!   second client asks for a record lock (op60) every 20 ms: how long the
//!   import takes and the longest any lock request waited (the commit holds
//!   the store's write lock, which op60 reads under).
//! - A grouped export of 4,000 records (Sort by Name, Export [Name, Total
//!   by Name] grouped by Name), without and with a 300 ms time limit.

#[path = "fixtures/tasks.rs"]
mod fixture;
#[path = "fixtures/server.rs"]
mod server;
#[path = "fixtures/transfer.rs"]
mod transfer;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fixture::{script, set_variable, TempFile, COUNT, NAME, OCCURRENCE, TABLE, TOTAL};
use om_hbam::slot::uint_segment;
use om_hbam::{serial, OwnedNode};
use om_scripts::specs::{SortEntry, SortSpec, SpecField};
use om_scripts::{flag, ScriptStep};
use om_server::ScriptLimits;
use om_transfer::spec::{ExportField, ExportOptions, FieldSpec};
use om_transfer::{ExportCharset, FileType};

/// A folder holding the hosted file and its Documents folder.
struct Folder(std::path::PathBuf);

impl Folder {
    fn new(tag: &str) -> Folder {
        let root = std::env::temp_dir().join(format!("om-timings-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Documents")).unwrap();
        Folder(root)
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn limits(duration: Duration) -> ScriptLimits {
    ScriptLimits {
        duration,
        ..ScriptLimits::DEFAULT
    }
}

#[test]
#[ignore]
fn an_import_of_20000_rows_and_other_clients_meanwhile() {
    const ROWS: usize = 20_000;
    let scripts = vec![script(
        1,
        "Import",
        vec![
            fixture::on(86, true),
            set_variable("$p", "Get ( DocumentsPath ) & \"rows.tab\""),
            transfer::import(&["$p"], &[NAME, COUNT], true),
            fixture::exit_with("Get ( LastError )"),
        ],
    )];
    let folder = Folder::new("import");
    let file = TempFile(folder.0.join("Tasks.fmp12"));
    fixture::write(&file.0, &scripts, true);
    let rows: String = (0..ROWS).map(|i| format!("Row {i}\t{i}\r")).collect();
    std::fs::write(folder.0.join("Documents").join("rows.tab"), rows).unwrap();
    let port = server::start_with_limits(&file, limits(Duration::from_secs(3600)));
    let (mut client, _) = server::logged_in(port, "importer");
    let (mut other, _) = server::logged_in(port, "other");

    let done = Arc::new(AtomicBool::new(false));
    let watcher = {
        let done = done.clone();
        std::thread::spawn(move || {
            let (table, record) = (uint_segment(TABLE), serial::encode(1));
            let path: [&[u8]; 3] = [&table, &[5], &record];
            let mut longest = Duration::ZERO;
            let mut requests = 0;
            while !done.load(Ordering::Acquire) {
                let asked = Instant::now();
                let answered = other.lock(&path, 7, true);
                longest = longest.max(asked.elapsed());
                if let Err(e) = answered {
                    // The client gave up waiting (its read timeout).
                    println!("op60 failed after {:.2?}: {e:?}", asked.elapsed());
                    break;
                }
                other.kill_all_locks(7).unwrap();
                requests += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            (longest, requests)
        })
    };
    let started = Instant::now();
    let reply = client.perform_script_on_server(1, "", true).unwrap();
    let took = started.elapsed();
    done.store(true, Ordering::Release);
    let (longest, requests) = watcher.join().unwrap();
    assert_eq!((reply.error, reply.result.as_str()), (0, "0"));
    let imported = server::records_of(&mut client, TABLE).len() - 2;
    assert_eq!(imported, ROWS);
    println!(
        "import of {ROWS} rows: {took:.2?}; longest op60 wait {longest:.2?} over {requests} requests"
    );
}

/// Sort Records [Restore; No dialog] by Tasks::Name.
fn sort_by_name() -> ScriptStep {
    let spec = SortSpec {
        keep_sorted: true,
        blanks_last: false,
        fields: vec![SortEntry {
            field: SpecField {
                occurrence: Some(OCCURRENCE),
                field: NAME,
                repetition: 1,
            },
            summary: None,
            value_list: None,
            descending: false,
            locale: 0,
        }],
    };
    let mut step = ScriptStep::new(39);
    step.set_data_leaf(0x0a, Some(OwnedNode::leaf([0x0a], spec.encode())));
    step.set_flag(flag::RESTORE, true);
    step.flags |= flag::MENU_SELECTION;
    step
}

#[test]
#[ignore]
fn a_grouped_export_of_4000_records() {
    const RECORDS: u64 = 4_000;
    let options = ExportOptions {
        fields: vec![
            ExportField {
                field: FieldSpec::new(OCCURRENCE, NAME),
                sum_by: FieldSpec::none(),
            },
            ExportField {
                field: FieldSpec::new(OCCURRENCE, TOTAL),
                sum_by: FieldSpec::new(OCCURRENCE, NAME),
            },
        ],
        group_by: vec![(FieldSpec::new(OCCURRENCE, NAME), 1)],
        formatting: false,
        charset: ExportCharset::Utf8,
        use_name_alias: Some(1),
        field_names: None,
    };
    let scripts = vec![script(
        1,
        "Export",
        vec![
            fixture::on(86, true),
            sort_by_name(),
            set_variable("$sorted", "Get ( LastError )"),
            set_variable("$p", "Get ( DocumentsPath ) & \"grouped.tab\""),
            transfer::export_with(&["$p"], options, FileType::TabSeparated),
            fixture::exit_with("$sorted & \"/\" & Get ( LastError )"),
        ],
    )];
    for limit in [Duration::from_secs(3600), Duration::from_millis(300)] {
        let folder = Folder::new("export");
        let mut root = fixture::tree(&scripts, true);
        fixture::add_tasks(&mut root, 3..=RECORDS);
        fixture::add_calculated(&mut root);
        let file = TempFile(folder.0.join("Tasks.fmp12"));
        fixture::write_tree(&file.0, &root);
        let port = server::start_with_limits(&file, limits(limit));
        let (mut client, _) = server::logged_in(port, "exporter");
        let started = Instant::now();
        let reply = client.perform_script_on_server(1, "", true).unwrap();
        let took = started.elapsed();
        let out = folder.0.join("Documents").join("grouped.tab");
        let lines = std::fs::read(&out)
            .map(|b| b.iter().filter(|c| **c == b'\r').count())
            .ok();
        println!(
            "grouped export of {RECORDS} records, limit {limit:?}: error {} ({:?}) in {took:.2?}; \
             file rows {lines:?}",
            reply.error, reply.result
        );
        // Finished (within the limit), or stopped by it without a file.
        match reply.error {
            0 => assert_eq!(lines, Some(RECORDS as usize)),
            1 => assert!(limit < Duration::from_secs(1) && lines.is_none()),
            other => panic!("error {other}"),
        }
    }
}

/// An import into a table with a unique field: each row's value is checked
/// against the rows staged before it (the review's P1, still open).
#[test]
#[ignore]
fn an_import_into_a_unique_field() {
    for rows in [2_000usize, 4_000, 8_000] {
        let scripts = vec![script(
            1,
            "Import",
            vec![
                fixture::on(86, true),
                set_variable("$p", "Get ( DocumentsPath ) & \"unique.tab\""),
                transfer::import(&["$p"], &[NAME, fixture::CODE], true),
                fixture::exit_with("Get ( LastError )"),
            ],
        )];
        let folder = Folder::new("unique");
        let mut root = fixture::tree(&scripts, true);
        // Code: unique, validated Always, with an English value index.
        fixture::set_field_validation(&mut root, TABLE, fixture::CODE, 0x414);
        fixture::set_field_options(&mut root, TABLE, fixture::CODE, 0x8000_0000);
        let file = TempFile(folder.0.join("Tasks.fmp12"));
        fixture::write_tree(&file.0, &root);
        // Codes of at most three characters (Code's validation).
        let source: String = (0..rows).map(|i| format!("Row {i}\t{:03x}\r", i)).collect();
        std::fs::write(folder.0.join("Documents").join("unique.tab"), source).unwrap();
        let port = server::start_with_limits(&file, limits(Duration::from_secs(3600)));
        let (mut client, _) = server::logged_in(port, "unique");
        let started = Instant::now();
        let reply = client.perform_script_on_server(1, "", true).unwrap();
        println!(
            "import of {rows} rows into a unique field: error {} in {:.2?}",
            reply.error,
            started.elapsed()
        );
    }
}
