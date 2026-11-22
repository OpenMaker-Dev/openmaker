//! End to end: Export Records and Import Records in server scripts
//! (script-steps.md 11.3-11.5, "Implementation notes (import/export)"),
//! over the Tasks file of `fixtures/tasks.rs`: the Documents folder
//! beside the hosted file, the session's temporary folder for relative
//! paths, error 800 outside them, imports through the file's rules.

#[path = "fixtures/tasks.rs"]
mod fixture;
#[path = "fixtures/server.rs"]
mod server;
#[path = "fixtures/transfer.rs"]
mod transfer;

use fixture::{script, set_variable, TempFile, CODE, COUNT, CREATOR, DOUBLED, DUE, NAME};
use om_hbam::OwnedNode;
use om_scripts::{flag, ScriptStep};
use om_session::Session;
use om_transfer::spec::ImportAction;
use om_transfer::FileType;
use transfer::{export, import, import_as, with_kinds};

fn run(session: &mut Session, script: u64) -> (u16, String) {
    let reply = session.perform_script_on_server(script, "", true).unwrap();
    (reply.error, reply.result)
}

#[test]
fn server_scripts_export_and_import_in_their_folders() {
    let tag =
        format!("{}-{:?}", std::process::id(), std::thread::current().id()).replace(['(', ')'], "");
    let out = format!("om-transfer-out-{tag}.tab");
    let source = format!("om-transfer-in-{tag}.tab");
    let state = "Get ( LastError ) & \"|\" & Get ( FoundCount ) & \"|\" & Get ( TotalRecordCount )";
    let scripts = vec![
        script(
            1,
            "Export",
            vec![
                fixture::on(86, true),
                set_variable("$p", &format!("Get ( DocumentsPath ) & \"{out}\"")),
                export(
                    &["$p"],
                    &[NAME, COUNT, DUE, DOUBLED],
                    FileType::TabSeparated,
                ),
                fixture::exit_with("Get ( LastError ) & \"|\" & Get ( DocumentsPath )"),
            ],
        ),
        script(
            2,
            "Import",
            vec![
                fixture::on(86, true),
                set_variable("$p", &format!("Get ( DocumentsPath ) & \"{source}\"")),
                import(&["$p"], &[NAME, COUNT, CODE], false),
                fixture::exit_with(state),
            ],
        ),
        script(
            3,
            "Outside",
            vec![
                fixture::on(86, true),
                export(&["/etc/om-transfer.tab"], &[NAME], FileType::TabSeparated),
                fixture::exit_with("Get ( LastError )"),
            ],
        ),
        script(
            4,
            "Relative",
            vec![
                fixture::on(86, true),
                export(
                    &["missing/x.tab", "rel.tab"],
                    &[NAME, COUNT],
                    FileType::TabSeparated,
                ),
                set_variable("$e", "Get ( LastError )"),
                import(&["rel.tab"], &[NAME, COUNT], true),
                fixture::exit_with(&format!("$e & \"/\" & {state}")),
            ],
        ),
        script(
            5,
            "Excel",
            vec![
                fixture::on(86, true),
                ScriptStep::new(143),
                fixture::exit_with("Get ( LastError )"),
            ],
        ),
    ];
    let file = TempFile::new("transfer", &scripts, true);
    let documents = file.0.parent().unwrap().join("Documents");
    std::fs::create_dir_all(&documents).unwrap();
    std::fs::write(documents.join(&source), "Alpha\t7\tab\rBeta\t8\ttoo long\r").unwrap();
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "client");

    let (error, result) = run(&mut client, 1);
    assert_eq!(error, 0, "{result}");
    // Get ( DocumentsPath ) spells the folder as the server was given it,
    // absolute and without resolving links (files.rs; on macOS the
    // temporary directory is /var/folders, a link to /private/var/folders),
    // and names the folder the export wrote to.
    let shown = std::path::absolute(&documents).unwrap();
    assert_eq!(result, format!("0|{}/", shown.display()));
    let path = result.strip_prefix("0|").unwrap();
    assert_eq!(
        std::fs::canonicalize(path).unwrap(),
        std::fs::canonicalize(&documents).unwrap()
    );
    let written = std::fs::read(documents.join(&out)).unwrap();
    assert_eq!(
        String::from_utf8(written).unwrap(),
        "Write tests\t3\t10/1/2026\t6\rShip\t5\t\t10\r"
    );

    // Beta's Code fails its "always" validation: left out, error 729; the
    // found set is the imported record.
    assert_eq!(run(&mut client, 2), (729, "729|1|3".into()));
    let records = server::records_of(&mut client, fixture::TABLE);
    let alpha = records
        .iter()
        .find(|(_, cells)| cells.iter().any(|(f, v)| *f == NAME && v == "Alpha"))
        .expect("Alpha imported");
    assert_eq!(server::cell(&records, alpha.0, COUNT), Some("7"));
    // Auto-enter was off: no creator.
    assert_eq!(server::cell(&records, alpha.0, CREATOR), None);
    assert!(!records
        .iter()
        .any(|(_, cells)| cells.iter().any(|(f, v)| *f == NAME && v == "Beta")));

    assert_eq!(run(&mut client, 3), (800, "800".into()));
    // A relative path is in the session's temporary folder; the first path
    // whose folder exists is used.
    assert_eq!(run(&mut client, 4), (0, "0/0|3|6".into()));
    // Save Records as Excel is not a server step.
    assert_eq!(run(&mut client, 5), (3, "3".into()));
    let _ = std::fs::remove_file(documents.join(&out));
    let _ = std::fs::remove_file(documents.join(&source));
}

fn run_with(session: &mut Session, script: u64, parameter: &str) -> (u16, String) {
    let reply = session
        .perform_script_on_server(script, parameter, true)
        .unwrap();
    (reply.error, reply.result)
}

/// A folder of its own holding the hosted file, so its Documents folder
/// is `<root>/Documents`, and a folder `<root>/outside` beside it; removed
/// when dropped.
struct Layout(std::path::PathBuf);

impl Layout {
    fn new(tag: &str) -> Layout {
        let root = std::env::temp_dir().join(format!(
            "om-transfer-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Documents")).unwrap();
        std::fs::create_dir_all(root.join("outside")).unwrap();
        Layout(root)
    }

    fn documents(&self, name: &str) -> String {
        format!("{}/Documents/{name}", self.0.display())
    }

    fn outside(&self, name: &str) -> std::path::PathBuf {
        self.0.join("outside").join(name)
    }
}

impl Drop for Layout {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn records_named(session: &mut Session, name: &str) -> usize {
    server::records_of(session, fixture::TABLE)
        .iter()
        .filter(|(_, cells)| cells.iter().any(|(f, v)| *f == NAME && v == name))
        .count()
}

/// The review's path probes (F1, F2): a path is used only when it names a
/// file beneath the Documents or the session's temporary folder without a
/// symbolic link anywhere below the folder; an export replaces the
/// destination's entry with a new file, so a hard link's other name keeps
/// its contents; an import refuses a file with another link.
#[cfg(unix)]
#[test]
fn server_script_paths_stay_in_their_folders() {
    use std::os::unix::fs::{symlink, MetadataExt};

    let layout = Layout::new("paths");
    let docs = layout.0.join("Documents");
    let outside = |name: &str| layout.outside(name);
    std::fs::create_dir_all(outside("deep")).unwrap();
    std::fs::create_dir_all(docs.join("sub")).unwrap();
    symlink(outside("deep"), docs.join("jump")).unwrap();
    symlink(outside("missing-dir"), docs.join("dlink")).unwrap();
    symlink(outside("dangling-target.tab"), docs.join("dangling.tab")).unwrap();
    std::fs::write(outside("hard-target.tab"), "SENTINEL").unwrap();
    std::fs::hard_link(outside("hard-target.tab"), docs.join("hard.tab")).unwrap();
    std::fs::write(outside("secret.tab"), "SECRET\r").unwrap();
    symlink(outside("secret.tab"), docs.join("ordinary.tab")).unwrap();
    std::fs::write(outside("linked.tab"), "LINKED\r").unwrap();
    std::fs::hard_link(outside("linked.tab"), docs.join("linked.tab")).unwrap();
    std::fs::write(docs.join("plain.tab"), "Plain\r").unwrap();
    let fifo = std::process::Command::new("mkfifo")
        .arg(docs.join("fifo.tab"))
        .status()
        .unwrap();
    assert!(fifo.success());

    let from_parameter = || set_variable("$p", "Get ( ScriptParameter )");
    let last_error = || fixture::exit_with("Get ( LastError )");
    let mut create_folders = export(&["$p"], &[NAME], FileType::TabSeparated);
    create_folders.flags |= flag::OPTION2;
    let mac = layout.documents("mac.tab");
    let win = layout.documents("win.tab");
    let linux = layout.documents("linux.tab");
    let scripts = vec![
        script(
            1,
            "Export",
            vec![
                fixture::on(86, true),
                from_parameter(),
                export(&["$p"], &[NAME], FileType::TabSeparated),
                last_error(),
            ],
        ),
        script(
            2,
            "Import",
            vec![
                fixture::on(86, true),
                from_parameter(),
                import(&["$p"], &[NAME], false),
                last_error(),
            ],
        ),
        script(
            3,
            "Export, create folders",
            vec![
                fixture::on(86, true),
                from_parameter(),
                create_folders,
                last_error(),
            ],
        ),
        script(
            4,
            "Temporary",
            vec![fixture::exit_with("Get ( TemporaryPath )")],
        ),
        script(
            5,
            "Other platforms",
            vec![
                fixture::on(86, true),
                with_kinds(
                    export(&[], &[NAME], FileType::TabSeparated),
                    &[("filemac", &mac), ("filewin", &win)],
                ),
                last_error(),
            ],
        ),
        script(
            6,
            "Other platforms, then Linux",
            vec![
                fixture::on(86, true),
                with_kinds(
                    export(&[], &[NAME], FileType::TabSeparated),
                    &[("filemac", &mac), ("filewin", &win), ("filelinux", &linux)],
                ),
                last_error(),
            ],
        ),
    ];
    let file = TempFile(layout.0.join("Tasks.fmp12"));
    fixture::write(&file.0, &scripts, true);
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "paths");
    let exported = "Write tests\rShip\r";
    let refused = (800, "800".to_owned());
    let missing = (100, "100".to_owned());

    // F1: `..` after a symbolic link to a folder outside.
    let escaped = layout.documents("jump/../escaped.tab");
    assert_eq!(run_with(&mut client, 1, &escaped), refused);
    assert!(!outside("escaped.tab").exists());
    assert!(!docs.join("escaped.tab").exists());
    assert_eq!(
        run_with(&mut client, 2, &layout.documents("jump/../secret.tab")),
        missing
    );
    assert_eq!(records_named(&mut client, "SECRET"), 0);
    // Through a linked folder, also creating folders there.
    assert_eq!(
        run_with(&mut client, 1, &layout.documents("jump/x.tab")),
        refused
    );
    assert_eq!(
        run_with(&mut client, 3, &layout.documents("jump/new/x.tab")),
        refused
    );
    assert!(!outside("deep/x.tab").exists() && !outside("deep/new").exists());
    assert_eq!(
        run_with(&mut client, 3, &layout.documents("dlink/x.tab")),
        refused
    );
    assert!(!outside("missing-dir").exists());

    // F1: a dangling symbolic link is not followed.
    assert_eq!(
        run_with(&mut client, 1, &layout.documents("dangling.tab")),
        refused
    );
    assert!(!outside("dangling-target.tab").exists());
    assert!(docs.join("dangling.tab").is_symlink());

    // F2: a hard link's other name keeps its contents; the export is a new
    // file under the Documents name.
    assert_eq!(
        run_with(&mut client, 1, &layout.documents("hard.tab")),
        (0, "0".into())
    );
    assert_eq!(
        std::fs::read_to_string(outside("hard-target.tab")).unwrap(),
        "SENTINEL"
    );
    assert_eq!(
        std::fs::read_to_string(docs.join("hard.tab")).unwrap(),
        exported
    );
    assert_eq!(std::fs::metadata(docs.join("hard.tab")).unwrap().nlink(), 1);
    // An import refuses a file with another link (error 100 as outside).
    assert_eq!(
        run_with(&mut client, 2, &layout.documents("linked.tab")),
        missing
    );
    assert_eq!(records_named(&mut client, "LINKED"), 0);

    // An ordinary symbolic link to a file outside stays refused.
    assert_eq!(
        run_with(&mut client, 1, &layout.documents("ordinary.tab")),
        refused
    );
    assert_eq!(
        std::fs::read_to_string(outside("secret.tab")).unwrap(),
        "SECRET\r"
    );
    assert!(docs.join("ordinary.tab").is_symlink());
    assert_eq!(
        run_with(&mut client, 2, &layout.documents("ordinary.tab")),
        missing
    );
    assert_eq!(records_named(&mut client, "SECRET"), 0);

    // A pipe is not a file (and does not block the session).
    assert_eq!(
        run_with(&mut client, 2, &layout.documents("fifo.tab")),
        missing
    );

    // Absolute paths: outside, climbing out of the folder, a folder.
    let abs = outside("abs.tab").display().to_string();
    assert_eq!(run_with(&mut client, 1, &abs), refused);
    assert_eq!(
        run_with(&mut client, 1, &layout.documents("../outside/abs.tab")),
        refused
    );
    assert!(!outside("abs.tab").exists());
    assert_eq!(run_with(&mut client, 1, &layout.documents("sub")), refused);
    assert_eq!(run_with(&mut client, 1, &layout.documents("")), refused);
    // `..` back into the folder through a real folder is fine.
    assert_eq!(
        run_with(&mut client, 1, &layout.documents("sub/../ok.tab")),
        (0, "0".into())
    );
    assert_eq!(
        std::fs::read_to_string(docs.join("ok.tab")).unwrap(),
        exported
    );

    // `filemac:` and `filewin:` lines are skipped; `filelinux:` is used.
    assert_eq!(run_with(&mut client, 5, ""), refused);
    assert_eq!(run_with(&mut client, 6, ""), (0, "0".into()));
    assert!(!docs.join("mac.tab").exists() && !docs.join("win.tab").exists());
    assert_eq!(
        std::fs::read_to_string(docs.join("linux.tab")).unwrap(),
        exported
    );
    assert_eq!(
        run_with(&mut client, 2, &layout.documents("plain.tab")),
        (0, "0".into())
    );
    assert_eq!(records_named(&mut client, "Plain"), 1);

    // A relative path climbing out of the temporary folder.
    assert_eq!(run_with(&mut client, 1, "../escape.tab"), refused);
    assert!(!std::env::temp_dir().join("escape.tab").exists());

    // Another session's temporary folder, by full and relative path: every
    // script runs in a session of its own (another client's, or this
    // client's earlier one).
    let (mut other, _) = server::logged_in(port, "other");
    let folders = [
        run_with(&mut other, 4, "").1,
        run_with(&mut client, 4, "").1,
    ];
    for folder in folders {
        let theirs = std::path::PathBuf::from(folder);
        let name = theirs.file_name().unwrap().to_str().unwrap().to_owned();
        std::fs::create_dir_all(&theirs).unwrap();
        std::fs::write(theirs.join("mine.tab"), "MINE\r").unwrap();
        let full = theirs.join("mine.tab").display().to_string();
        assert_eq!(run_with(&mut client, 2, &full), missing);
        assert_eq!(
            run_with(&mut client, 2, &format!("../{name}/mine.tab")),
            missing
        );
        let x = theirs.join("x.tab");
        assert_eq!(run_with(&mut client, 1, &x.display().to_string()), refused);
        assert!(!x.exists());
        std::fs::remove_dir_all(&theirs).unwrap();
    }
    assert_eq!(records_named(&mut client, "MINE"), 0);
}

/// Fields the import-rule tests add to Tasks: Tag, a stored calculation
/// `Creator & "/" & Name`; Modifier, auto-entered modification account
/// name; Stamp, a stored calculation `Modifier & ":" & Count`.
const TAG: u64 = 20;
const MODIFIER: u64 = 21;
const STAMP: u64 = 22;

/// Tasks with [`TAG`], [`MODIFIER`] and [`STAMP`].
fn rules_tree(scripts: &[om_scripts::Script]) -> OwnedNode {
    let mut root = fixture::tree(scripts, true);
    let mut names = fixture::names();
    for (id, name) in [(TAG, "Tag"), (MODIFIER, "Modifier"), (STAMP, "Stamp")] {
        names.add_field(fixture::TABLE, id, name);
    }
    fixture::add_field(&mut root, fixture::TABLE, TAG, "Tag", 2, 1, 0, 0, 0);
    // Modification account name (variable 9), auto-entered.
    fixture::add_field(
        &mut root,
        fixture::TABLE,
        MODIFIER,
        "Modifier",
        1,
        1,
        9,
        0x1,
        0,
    );
    fixture::add_field(&mut root, fixture::TABLE, STAMP, "Stamp", 2, 1, 0, 0, 0);
    let at = (fixture::TABLE, fixture::OCCURRENCE);
    fixture::add_formula(&mut root, &names, at, TAG, 5, "Creator & \"/\" & Name");
    fixture::add_formula(&mut root, &names, at, STAMP, 5, "Modifier & \":\" & Count");
    root
}

/// The record of Tasks whose Name is `name`, from [`server::records_of`].
fn record_named(session: &mut Session, name: &str) -> Option<(u64, Vec<(u64, String)>)> {
    server::records_of(session, fixture::TABLE)
        .into_iter()
        .find(|(_, cells)| cells.iter().any(|(f, v)| *f == NAME && v == name))
}

fn value(record: &(u64, Vec<(u64, String)>), field: u64) -> Option<&str> {
    record
        .1
        .iter()
        .find(|(f, _)| *f == field)
        .map(|(_, v)| v.as_str())
}

/// The review's C1: an imported row is stored with what the import's rules
/// gave it. Creator is auto-entered and validated not empty Always, so a
/// commit that forgot the auto-entered value refused every row (509), and
/// a stored calculation over it read it empty.
#[test]
fn imported_rows_keep_the_values_auto_enter_gave_them() {
    let source = "rules-add.tab";
    let scripts = vec![script(
        1,
        "Add, auto-enter",
        vec![
            fixture::on(86, true),
            set_variable("$p", &format!("Get ( DocumentsPath ) & \"{source}\"")),
            import(&["$p"], &[NAME, COUNT], true),
            fixture::exit_with("Get ( LastError ) & \"|\" & Get ( FoundCount )"),
        ],
    )];
    let layout = Layout::new("rules-add");
    let mut root = rules_tree(&scripts);
    fixture::set_field_validation(&mut root, fixture::TABLE, CREATOR, 0x408);
    let file = TempFile(layout.0.join("Tasks.fmp12"));
    fixture::write_tree(&file.0, &root);
    std::fs::write(
        layout.0.join("Documents").join(source),
        "Alpha\t1\rBeta\t2\r",
    )
    .unwrap();
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "rules-add");

    assert_eq!(run(&mut client, 1), (0, "0|2".into()));
    for name in ["Alpha", "Beta"] {
        let record = record_named(&mut client, name).expect("imported");
        assert_eq!(value(&record, CREATOR), Some("Admin"), "{name}");
        // The stored calculation and its index see the auto-entered value.
        assert_eq!(value(&record, TAG), Some(format!("Admin/{name}").as_str()));
        assert_eq!(value(&record, MODIFIER), Some("Admin"));
    }
}

#[test]
fn import_into_a_duplicate_protects_only_mapped_values_from_auto_entry() {
    use fixture::{OCCURRENCE, TABLE};
    use om_hbam::{serial, slot::uint_segment};

    const NUMBER: u64 = 41;
    let duplicate_and_import = |id, name, source, fields: &[u64], auto_enter| {
        script(
            id,
            name,
            vec![
                fixture::on(86, true),
                set_variable("$p", &format!("Get ( DocumentsPath ) & \"{source}\"")),
                ScriptStep::new(23),
                fixture::go_to_record(1),
                ScriptStep::new(205),
                ScriptStep::new(8),
                // Replace the found set with just the duplicate.
                ScriptStep::new(25),
                ScriptStep::new(27),
                import_as(&["$p"], fields, auto_enter, ImportAction::UpdateFoundSet),
                ScriptStep::new(206),
                fixture::exit_with("Get ( LastError )"),
            ],
        )
    };
    let scripts = [
        duplicate_and_import(1, "Unmapped Number", "copy.tab", &[NAME], true),
        duplicate_and_import(2, "Auto-entry off", "copy.tab", &[NAME], false),
        duplicate_and_import(3, "Mapped Number", "mapped.tab", &[NAME, NUMBER], true),
    ];
    let mut root = fixture::tree(&scripts, true);
    fixture::add_field(&mut root, TABLE, NUMBER, "Number", 1, 1, 0, 0x88, 0);
    fixture::add_formula(
        &mut root,
        &fixture::names(),
        (TABLE, OCCURRENCE),
        NUMBER,
        5,
        "Get ( RecordNumber )",
    );
    root.put(
        &[
            &uint_segment(TABLE),
            &[5],
            &serial::encode(1),
            &uint_segment(NUMBER),
        ],
        om_scripts::text_node(&[], "1"),
    );
    let layout = Layout::new("duplicate-import");
    let file = TempFile(layout.0.join("Tasks.fmp12"));
    fixture::write_tree(&file.0, &root);
    std::fs::write(layout.documents("copy.tab"), "Imported copy\r").unwrap();
    std::fs::write(layout.documents("mapped.tab"), "Mapped copy\t99\r").unwrap();
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "duplicate-import");

    let before = server::records_of(&mut client, TABLE);
    assert_eq!(server::cell(&before, 1, NUMBER), Some("1"));
    // The unmapped calculation needs a record number the import cannot
    // supply. A refused row gives 729, and Commit Transaction rolls back.
    assert_eq!(run(&mut client, 1).1, "729");
    assert_eq!(server::records_of(&mut client, TABLE), before);

    assert_eq!(run(&mut client, 2), (0, "0".into()));
    let copy = record_named(&mut client, "Imported copy").expect("duplicate committed");
    assert_ne!(copy.0, 1);
    assert_eq!(value(&copy, NUMBER), Some("1"));

    assert_eq!(run(&mut client, 3), (0, "0".into()));
    let copy = record_named(&mut client, "Mapped copy").expect("duplicate committed");
    assert_ne!(copy.0, 1);
    assert_eq!(value(&copy, NUMBER), Some("99"));
}

/// The review's C1 for updates, C2 and C3: an update's stored calculation
/// reads the new modification value; inside Open Transaction, Commit
/// Transaction neither auto-enters what the import left out nor checks
/// "only during data entry" validation the import does not check.
#[test]
fn imports_keep_their_rules_in_updates_and_transactions() {
    let staged = |n: &str| format!("Get ( DocumentsPath ) & \"rules-{n}.tab\"");
    let errors = "$i & \"/\" & Get ( LastError )";
    let scripts = vec![
        script(
            1,
            "Update, auto-enter",
            vec![
                fixture::on(86, true),
                set_variable("$p", &staged("update")),
                import_as(&["$p"], &[COUNT], true, ImportAction::UpdateFoundSet),
                fixture::exit_with("Get ( LastError )"),
            ],
        ),
        script(
            2,
            "In a transaction, auto-enter off",
            vec![
                fixture::on(86, true),
                set_variable("$p", &staged("off")),
                ScriptStep::new(205),
                import(&["$p"], &[NAME, COUNT], false),
                set_variable("$i", "Get ( LastError )"),
                ScriptStep::new(206),
                fixture::exit_with(errors),
            ],
        ),
        script(
            3,
            "In a transaction, data entry only",
            vec![
                fixture::on(86, true),
                set_variable("$p", &staged("entry")),
                ScriptStep::new(205),
                fixture::go_to_record(1),
                fixture::set_field(NAME, "\"Edited in tx\""),
                import(&["$p"], &[NAME, COUNT, CODE], false),
                set_variable("$i", "Get ( LastError )"),
                ScriptStep::new(206),
                fixture::exit_with(errors),
            ],
        ),
    ];
    let layout = Layout::new("rules-tx");
    let mut root = rules_tree(&scripts);
    // Code: by calculation, only during data entry.
    fixture::set_field_validation(&mut root, fixture::TABLE, CODE, 0x001);
    let file = TempFile(layout.0.join("Tasks.fmp12"));
    fixture::write_tree(&file.0, &root);
    let documents = layout.0.join("Documents");
    std::fs::write(documents.join("rules-update.tab"), "7\r").unwrap();
    std::fs::write(documents.join("rules-off.tab"), "Gamma\t1\r").unwrap();
    std::fs::write(documents.join("rules-entry.tab"), "Delta\t7\ttoo long\r").unwrap();
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "rules-tx");

    // C1, an update: Stamp reads the new Modifier and Count.
    assert_eq!(run(&mut client, 1), (0, "0".into()));
    let first = record_named(&mut client, "Write tests").unwrap();
    assert_eq!(value(&first, COUNT), Some("7"));
    assert_eq!(value(&first, MODIFIER), Some("Admin"));
    assert_eq!(value(&first, STAMP), Some("Admin:7"));

    // C2: auto-enter off stays off at Commit Transaction.
    assert_eq!(run(&mut client, 2), (0, "0/0".into()));
    let gamma = record_named(&mut client, "Gamma").expect("imported");
    assert_eq!(value(&gamma, CREATOR), None);
    assert_eq!(value(&gamma, MODIFIER), None);
    assert_eq!(value(&gamma, TAG), Some("/Gamma"));

    // C3: the value the import accepted is not refused at Commit
    // Transaction, and the script's own edit commits with it.
    assert_eq!(run(&mut client, 3), (0, "0/0".into()));
    assert!(record_named(&mut client, "Edited in tx").is_some());
    let delta = record_named(&mut client, "Delta").expect("imported");
    assert_eq!(value(&delta, CODE), Some("too long"));
}

/// The review's P5: a table whose rules the server cannot enforce (serial
/// auto-entry) refuses the import step with 3, as other script writes to
/// it, instead of refusing every row (729).
#[test]
fn imports_into_tables_the_server_cannot_write_fail_with_3() {
    let scripts = vec![script(
        1,
        "Import",
        vec![
            fixture::on(86, true),
            set_variable("$p", "Get ( DocumentsPath ) & \"serial.tab\""),
            import(&["$p"], &[NAME], true),
            fixture::exit_with("Get ( LastError )"),
        ],
    )];
    let layout = Layout::new("serial");
    let mut root = fixture::tree(&scripts, true);
    // Code: serial auto-entry.
    fixture::set_field_options(&mut root, fixture::TABLE, CODE, 0x2);
    let file = TempFile(layout.0.join("Tasks.fmp12"));
    fixture::write_tree(&file.0, &root);
    std::fs::write(layout.0.join("Documents").join("serial.tab"), "Alpha\r").unwrap();
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "serial");
    assert_eq!(run(&mut client, 1), (3, "3".into()));
    assert_eq!(records_named(&mut client, "Alpha"), 0);
}

/// An import into a field that must be a member of a value list of a
/// field's values, that field indexed in a language OpenMaker does not key
/// (German, #571): the row with a value to check ends the import with 3
/// and nothing is imported, the row before it neither, rather than
/// skipping that row (729) and keeping the others.
#[test]
fn imports_checked_against_an_unkeyed_value_list_fail_with_3() {
    let scripts = vec![script(
        1,
        "Import",
        vec![
            fixture::on(86, true),
            set_variable("$p", "Get ( DocumentsPath ) & \"codes.tab\""),
            import(&["$p"], &[NAME, CODE], true),
            fixture::exit_with("Get ( LastError )"),
        ],
    )];
    let layout = Layout::new("unkeyed-list");
    let mut root = fixture::tree(&scripts, true);
    fixture::set_field_language(&mut root, fixture::TABLE, NAME, 25);
    fixture::code_in_names_list(&mut root);
    let file = TempFile(layout.0.join("Tasks.fmp12"));
    fixture::write_tree(&file.0, &root);
    std::fs::write(
        layout.0.join("Documents").join("codes.tab"),
        "Blank\t\rShipped\tShip\r",
    )
    .unwrap();
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "unkeyed-list");
    assert_eq!(run(&mut client, 1), (3, "3".into()));
    assert_eq!(records_named(&mut client, "Blank"), 0);
    assert_eq!(records_named(&mut client, "Shipped"), 0);
}

/// The review's C4 and C5: an import or export the script's time limit
/// (or an administrator) aborts stops at the next row or record with
/// error 1, and writes nothing: no record of the import, also inside the
/// script's transaction, and no export file.
#[test]
fn imports_and_exports_stop_at_the_scripts_time_limit() {
    const ROWS: usize = 60_000;
    const RECORDS: u64 = 60_000;
    let documents = |name: &str| format!("Get ( DocumentsPath ) & \"{name}\"");
    let scripts = vec![
        script(
            1,
            "Import",
            vec![
                fixture::on(86, true),
                set_variable("$p", &documents("limit-in.tab")),
                import(&["$p"], &[NAME, COUNT], true),
                fixture::exit_with("Get ( LastError )"),
            ],
        ),
        script(
            2,
            "Import in a transaction",
            vec![
                fixture::on(86, true),
                set_variable("$p", &documents("limit-in.tab")),
                ScriptStep::new(205),
                fixture::go_to_record(1),
                fixture::set_field(NAME, "\"Changed in tx\""),
                import(&["$p"], &[NAME, COUNT], true),
                ScriptStep::new(206),
                fixture::exit_with("Get ( LastError )"),
            ],
        ),
        script(
            3,
            "Export",
            vec![
                fixture::on(86, true),
                set_variable("$p", &documents("limit-out.tab")),
                export(&["$p"], &[NAME, COUNT, DOUBLED], FileType::TabSeparated),
                fixture::exit_with("Get ( LastError )"),
            ],
        ),
    ];
    let layout = Layout::new("limits");
    let mut root = fixture::tree(&scripts, true);
    fixture::add_tasks(&mut root, 3..=RECORDS);
    let file = TempFile(layout.0.join("Tasks.fmp12"));
    fixture::write_tree(&file.0, &root);
    let rows: String = (0..ROWS).map(|i| format!("Row {i}\t{i}\r")).collect();
    std::fs::write(layout.0.join("Documents").join("limit-in.tab"), rows).unwrap();
    let limit = std::time::Duration::from_millis(300);
    let port = server::start_with_limits(
        &file,
        om_server::ScriptLimits {
            duration: limit,
            ..om_server::ScriptLimits::DEFAULT
        },
    );
    let (mut client, _) = server::logged_in(port, "limits");
    let written = |client: &mut Session| {
        server::records_of(client, fixture::TABLE)
            .iter()
            .filter(|(_, cells)| {
                cells
                    .iter()
                    .any(|(f, v)| *f == NAME && (v.starts_with("Row ") || v == "Changed in tx"))
            })
            .count()
    };
    for script in [1, 2, 3] {
        let started = std::time::Instant::now();
        assert_eq!(run(&mut client, script), (1, "".into()), "script {script}");
        // The step stopped soon after the limit (debug builds are slow;
        // before, a 20,000-row import ran on for minutes).
        let took = started.elapsed();
        assert!(took < limit * 20, "script {script} took {took:?}");
        assert_eq!(written(&mut client), 0, "script {script}");
    }
    assert!(!layout.0.join("Documents").join("limit-out.tab").exists());
}

/// Sort Records [Restore; No dialog] by Tasks fields, ascending.
fn sort(fields: &[u64]) -> ScriptStep {
    use om_scripts::specs::{SortEntry, SortSpec, SpecField};
    let spec = SortSpec {
        keep_sorted: true,
        blanks_last: false,
        fields: fields
            .iter()
            .map(|&field| SortEntry {
                field: SpecField {
                    occurrence: Some(fixture::OCCURRENCE),
                    field,
                    repetition: 1,
                },
                summary: None,
                value_list: None,
                descending: false,
                locale: 0,
            })
            .collect(),
    };
    let mut step = ScriptStep::new(39);
    step.set_data_leaf(0x0a, Some(OwnedNode::leaf([0x0a], spec.encode())));
    step.set_flag(flag::RESTORE, true);
    step.flags |= flag::MENU_SELECTION;
    step
}

/// A grouped export's summaries, each group's computed once over the
/// sorted found set (the review's C5): a total, a running total restarting
/// at Status and a fraction subtotaled by Status, each by Status, on the
/// row starting its group; the grand total on the first row. Then, on a
/// copy whose running total does not restart and whose fraction is not
/// subtotaled, the grand summaries without a break field as FileMaker Pro
/// 26.0.3 wrote them on the first row only (#373).
#[test]
fn grouped_exports_summarize_each_group() {
    use fixture::{RUNNING, SHARE, STATUS, TOTAL};
    use om_transfer::spec::{ExportField, ExportOptions, FieldSpec};
    let at = |field| FieldSpec::new(fixture::OCCURRENCE, field);
    let column = |field, by: Option<u64>| ExportField {
        field: at(field),
        sum_by: by.map_or_else(FieldSpec::none, at),
    };
    let options = ExportOptions {
        fields: vec![
            column(STATUS, None),
            column(NAME, None),
            column(TOTAL, Some(STATUS)),
            column(RUNNING, Some(STATUS)),
            column(SHARE, Some(STATUS)),
            column(TOTAL, None),
        ],
        group_by: vec![(at(STATUS), 1)],
        formatting: false,
        charset: om_transfer::ExportCharset::Utf8,
        use_name_alias: Some(1),
        field_names: None,
    };
    let scripts = vec![script(
        1,
        "Export",
        vec![
            fixture::on(86, true),
            sort(&[STATUS, NAME]),
            set_variable("$p", "Get ( DocumentsPath ) & \"grouped.tab\""),
            transfer::export_with(&["$p"], options, FileType::TabSeparated),
            fixture::exit_with("Get ( LastError )"),
        ],
    )];
    let layout = Layout::new("grouped");
    let mut root = fixture::tree(&scripts, true);
    fixture::add_tasks(&mut root, 3..=6);
    fixture::add_calculated(&mut root);
    let t = om_hbam::slot::uint_segment(fixture::TABLE);
    for id in 1..=6u64 {
        let status = if id % 2 == 1 { "Open" } else { "Done" };
        root.put(
            &[
                &t,
                &[5],
                &om_hbam::serial::encode(id),
                &om_hbam::slot::uint_segment(STATUS),
            ],
            om_scripts::text_node(&[], status),
        );
    }
    let file = TempFile(layout.0.join("Tasks.fmp12"));
    fixture::write_tree(&file.0, &root);
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "grouped");
    assert_eq!(run(&mut client, 1), (0, "0".into()));
    // Done: Ship 5, Task 4 4, Task 6 6; Open: Task 3 3, Task 5 5, Write
    // tests 3.
    assert_eq!(
        std::fs::read_to_string(layout.0.join("Documents").join("grouped.tab")).unwrap(),
        "Done\tShip\t15\t15\t1\t26\rOpen\tTask 3\t11\t11\t1\t\r"
    );

    // The native case: values 10, 20, 30 in groups A, A, B, sorted by the
    // group field only; Group, Total, Running Total, Fraction.
    let options = ExportOptions {
        fields: vec![
            column(STATUS, None),
            column(TOTAL, None),
            column(RUNNING, None),
            column(SHARE, None),
        ],
        group_by: vec![(at(STATUS), 1)],
        formatting: false,
        charset: om_transfer::ExportCharset::Utf8,
        use_name_alias: Some(1),
        field_names: None,
    };
    let scripts = vec![script(
        1,
        "Export",
        vec![
            fixture::on(86, true),
            sort(&[STATUS]),
            set_variable("$p", "Get ( DocumentsPath ) & \"grand.tab\""),
            transfer::export_with(&["$p"], options, FileType::TabSeparated),
            fixture::exit_with("Get ( LastError )"),
        ],
    )];
    let plain = Layout::new("grouped-grand");
    let mut root = fixture::tree(&scripts, true);
    fixture::add_tasks(&mut root, 3..=3);
    fixture::add_calculated(&mut root);
    // Running total with restart off, fraction of total with subtotal off.
    for (field, operation, flags) in [(RUNNING, 1, 0x04), (SHARE, 7, 0)] {
        root.put(
            &[&t, &[3], &[5], &om_hbam::slot::uint_segment(field), &[9]],
            OwnedNode::leaf(
                Vec::new(),
                fixture::summary_definition(operation, flags, COUNT, None),
            ),
        );
    }
    for (id, status, count) in [(1u64, "A", "10"), (2, "A", "20"), (3, "B", "30")] {
        for (field, text) in [(STATUS, status), (COUNT, count)] {
            root.put(
                &[
                    &t,
                    &[5],
                    &om_hbam::serial::encode(id),
                    &om_hbam::slot::uint_segment(field),
                ],
                om_scripts::text_node(&[], text),
            );
        }
    }
    let file = TempFile(plain.0.join("Tasks.fmp12"));
    fixture::write_tree(&file.0, &root);
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "grouped-grand");
    assert_eq!(run(&mut client, 1), (0, "0".into()));
    assert_eq!(
        std::fs::read_to_string(plain.0.join("Documents").join("grand.tab")).unwrap(),
        "A\t60\t60\t1\rB\t\t\t\r"
    );
}

/// The review's P2, as FileMaker Server answered (oracle x04): an update
/// of the found set into another table than the window's works on that
/// table's records in creation order (rows past them added with "add
/// remaining"), and the window's found set stays as it was.
#[test]
fn updates_into_another_table_work_on_its_records_in_creation_order() {
    use om_transfer::spec::import_flag;
    let mut step = transfer::import_into(
        fixture::NOTES_OCCURRENCE,
        &["$p"],
        &[fixture::BODY],
        false,
        ImportAction::UpdateFoundSet,
    );
    // "Add remaining data as new records".
    let mut options = om_transfer::ImportOptions::decode(
        step.data_leaf(0x84)
            .and_then(|n| n.value.as_deref())
            .unwrap(),
    )
    .unwrap();
    options.flags |= import_flag::ADD_REMAINING;
    step.set_data_leaf(0x84, Some(OwnedNode::leaf([0x84], options.encode())));
    let scripts = vec![script(
        1,
        "Replace notes",
        vec![
            fixture::on(86, true),
            set_variable("$p", "Get ( DocumentsPath ) & \"notes.tab\""),
            step,
            fixture::exit_with("Get ( LastError ) & \"|\" & Get ( FoundCount )"),
        ],
    )];
    let layout = Layout::new("other-table");
    let file = TempFile(layout.0.join("Tasks.fmp12"));
    fixture::write(&file.0, &scripts, true);
    std::fs::write(layout.0.join("Documents").join("notes.tab"), "One\rTwo\r").unwrap();
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "other-table");
    assert_eq!(run(&mut client, 1), (0, "0|2".into()));
    let notes = server::records_of(&mut client, fixture::NOTES);
    let bodies: Vec<&str> = notes
        .iter()
        .filter_map(|(id, _)| server::cell(&notes, *id, fixture::BODY))
        .collect();
    assert_eq!(bodies, ["One", "Two"]);
}

/// Unique values among imported rows, through the transaction's index of
/// its records' values: a row repeating an earlier row's value, or a
/// stored record's (case does not matter), is refused (729 at the end);
/// the others are imported.
#[test]
fn imported_rows_keep_unique_values_unique() {
    let scripts = vec![script(
        1,
        "Import",
        vec![
            fixture::on(86, true),
            set_variable("$p", "Get ( DocumentsPath ) & \"unique.tab\""),
            import(&["$p"], &[NAME, CODE], true),
            fixture::exit_with("Get ( LastError ) & \"|\" & Get ( FoundCount )"),
        ],
    )];
    let layout = Layout::new("unique-rows");
    let mut root = fixture::tree(&scripts, true);
    // Code: unique, validated Always, with an English value index; record
    // 1 holds "ab".
    fixture::set_field_validation(&mut root, fixture::TABLE, CODE, 0x414);
    fixture::set_field_options(&mut root, fixture::TABLE, CODE, 0x8000_0000);
    root.put(
        &[
            &om_hbam::slot::uint_segment(fixture::TABLE),
            &[5],
            &om_hbam::serial::encode(1),
            &om_hbam::slot::uint_segment(CODE),
        ],
        om_scripts::text_node(&[], "ab"),
    );
    let file = TempFile(layout.0.join("Tasks.fmp12"));
    fixture::write_tree(&file.0, &root);
    std::fs::write(
        layout.0.join("Documents").join("unique.tab"),
        "One\tx1\rTwo\tX1\rThree\tAB\rFour\tx4\r",
    )
    .unwrap();
    let port = server::start(&file);
    let (mut client, _) = server::logged_in(port, "unique-rows");
    assert_eq!(run(&mut client, 1), (729, "729|2".into()));
    for (name, imported) in [
        ("One", true),
        ("Two", false),
        ("Three", false),
        ("Four", true),
    ] {
        assert_eq!(
            record_named(&mut client, name).is_some(),
            imported,
            "{name}"
        );
    }
}

/// A data file step naming `paths` (variables expanded by the runtime).
fn file_step(id: u16, paths: &[&str]) -> ScriptStep {
    transfer::with_paths(ScriptStep::new(id), paths)
}

/// A step whose slot 0 is the file id `$id`, and slot 1 `second`.
fn on_file(id: u16, second: Option<&str>) -> ScriptStep {
    let mut step = ScriptStep::new(id);
    step.set_calculation(0, Some(&fixture::calc("$id")));
    if let Some(text) = second {
        step.set_calculation(1, Some(&fixture::calc(text)));
    }
    step
}

fn into(mut step: ScriptStep, variable: &str) -> ScriptStep {
    step.set_variable_target(0, variable);
    step
}

/// The files this process has open whose path holds `tag`.
#[cfg(target_os = "linux")]
fn open_files_naming(tag: &str) -> Vec<String> {
    std::fs::read_dir("/proc/self/fd")
        .unwrap()
        .flatten()
        .filter_map(|fd| std::fs::read_link(fd.path()).ok())
        .map(|path| path.to_string_lossy().into_owned())
        .filter(|path| path.contains(tag))
        .collect()
}

/// The data file steps in server scripts (#255): beneath the Documents and
/// temporary folders, with FileMaker's errors, and the session's files
/// closed when its script ends, also at an abort.
#[cfg(unix)]
#[test]
fn server_scripts_use_data_files_in_their_folders() {
    let tag = format!(
        "om-data-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    )
    .replace(['(', ')'], "");
    let error = |name: &str| set_variable(name, "Get ( LastError )");
    let mut write = into(on_file(192, None), "$text");
    write.value = 2;
    write.flags |= flag::OPTION2;
    let mut read = into(on_file(193, Some("5")), "$read");
    read.value = 2;
    let mut past_end = on_file(193, Some("1"));
    past_end.value = 2;
    let mut rename_up = file_step(199, &["$p"]);
    rename_up.set_calculation(0, Some(&fixture::calc("\"../up.txt\"")));
    let mut rename = file_step(199, &["$p"]);
    rename.set_calculation(0, Some(&fixture::calc(&format!("\"{tag}-2.txt\""))));
    let scripts = vec![
        script(
            1,
            "Data files",
            vec![
                fixture::on(86, true),
                set_variable("$p", &format!("Get ( DocumentsPath ) & \"{tag}.txt\"")),
                set_variable("$p2", &format!("Get ( DocumentsPath ) & \"{tag}-2.txt\"")),
                set_variable("$text", "\"Hello files\""),
                file_step(190, &["$p"]),
                set_variable("$docsPath", "Get ( DocumentsPath )"),
                set_variable("$tempPath", "Get ( TemporaryPath )"),
                into(file_step(188, &["$docsPath"]), "$docs"),
                into(file_step(188, &["$tempPath"]), "$temp"),
                set_variable("$folder", "Get ( DocumentsPath ) & \"logs/\""),
                into(file_step(188, &["$folder"]), "$folderExists"),
                set_variable("$fileSlash", "$p & \"/\""),
                into(file_step(188, &["$fileSlash"]), "$fileFolder"),
                set_variable("$outsideFolder", "Get ( DocumentsPath ) & \"../\""),
                into(file_step(188, &["$outsideFolder"]), "$outsideExists"),
                into(file_step(191, &["$p"]), "$id"),
                write,
                // An open file is not deleted.
                file_step(197, &["$p"]),
                error("$inuse"),
                on_file(195, Some("6")),
                read,
                into(on_file(194, None), "$pos"),
                on_file(195, Some("12")),
                past_end,
                error("$end"),
                set_variable("$info", "Get ( OpenDataFileInfo )"),
                on_file(196, None),
                into(file_step(189, &["$p"]), "$size"),
                rename_up,
                error("$badname"),
                rename,
                into(file_step(188, &["$p2"]), "$exists"),
                file_step(191, &["$p"]),
                error("$missing"),
                set_variable("$out", "Get ( DocumentsPath ) & \"../outside.txt\""),
                file_step(190, &["$out"]),
                error("$outside"),
                file_step(197, &["$p2"]),
                into(file_step(188, &["$p2"]), "$deleted"),
                // Left open: the session closes it.
                set_variable("$k", &format!("\"{tag}-kept.txt\"")),
                file_step(190, &["$k"]),
                into(file_step(191, &["$k"]), "$kept"),
                fixture::exit_with(&format!(
                    "$read & \"|\" & $pos & \"|\" & $end & \"|\" & $inuse & \"|\" & \
                     PatternCount ( $info ; \"1\t\" ) & \"|\" & \
                     PatternCount ( $info ; \"Documents/{tag}.txt\" ) & \"|\" & $size & \
                     \"|\" & $badname & \"|\" & $exists & \"|\" & $missing & \"|\" & \
                     $outside & \"|\" & $deleted & \"|\" & $kept & \"|\" & Get ( LastError ) & \"|\" & $docs & \"|\" & $temp & \"|\" & $folderExists & \"|\" & $fileFolder & \"|\" & $outsideExists"
                )),
            ],
        ),
        script(
            2,
            "Data file left open at an abort",
            vec![
                fixture::on(86, true),
                set_variable("$k", &format!("\"{tag}-aborted.txt\"")),
                file_step(190, &["$k"]),
                into(file_step(191, &["$k"]), "$id"),
                ScriptStep::new(71),
                set_variable("$x", "1"),
                ScriptStep::new(73),
            ],
        ),
    ];
    let layout = Layout::new("data-files");
    std::fs::create_dir_all(layout.0.join("Documents/logs")).unwrap();
    let root = fixture::tree(&scripts, true);
    let file = TempFile(layout.0.join("Tasks.fmp12"));
    fixture::write_tree(&file.0, &root);
    let limit = std::time::Duration::from_secs(3);
    let port = server::start_with_limits(
        &file,
        om_server::ScriptLimits {
            duration: limit,
            ..om_server::ScriptLimits::DEFAULT
        },
    );
    let (mut client, _) = server::logged_in(port, "data-files");
    let (code, result) = run(&mut client, 1);
    assert_eq!(code, 0, "{result}");
    let fields: Vec<&str> = result.split('|').collect();
    // "Hello files" and a line feed are 12 bytes: 5 from position 6 are
    // "files", leaving it at 11; nothing is past 12 (10).
    assert_eq!(fields[..4], ["files", "11", "10", "300"], "{result}");
    // Get ( OpenDataFileInfo ) gave id 1 and the file in the Documents
    // folder.
    assert_eq!(fields[4..6], ["1", "1"], "{result}");
    // Size 12; a new name with a folder is 826; renamed, it exists; the old
    // name is missing (100); outside the folders Create is 800; deleted,
    // it is gone; the file left open had id 2; the script ended without
    // an error.
    assert_eq!(
        fields[6..14],
        ["12", "826", "1", "100", "800", "0", "2", "0"],
        "{result}"
    );
    // Folder paths accept their trailing slash and permitted roots. A
    // file with a slash and a traversal outside the roots still refuse.
    assert_eq!(fields[14..], ["1", "1", "1", "0", "0"], "{result}");
    let documents = layout.0.join("Documents");
    assert!(!documents.join(format!("{tag}.txt")).exists());
    assert!(!documents.join(format!("{tag}-2.txt")).exists());
    assert!(!layout.0.join("outside.txt").exists());
    #[cfg(target_os = "linux")]
    assert_eq!(open_files_naming(&tag), Vec::<String>::new());

    // The time limit aborts a script with a file open: the session closes
    // it as it ends.
    let started = std::time::Instant::now();
    assert_eq!(run(&mut client, 2), (1, String::new()));
    assert!(started.elapsed() < limit * 10, "{:?}", started.elapsed());
    #[cfg(target_os = "linux")]
    assert_eq!(open_files_naming(&tag), Vec::<String>::new());
    client.disconnect().unwrap();
}
