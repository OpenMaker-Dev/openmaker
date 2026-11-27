//! Backups (docs/re/backups.md; docs/admin-console.md "Backups") on an
//! `om-server` running in this process with a clock the test moves: a
//! backup's folder as FileMaker Server lays it out, copies that read back
//! byte for byte and open on a server, unchanged files linked, container
//! data copied without its links, retention past the keep count sparing
//! preserved backups and everything that is not the schedule's own, and
//! copies taken while a client writes that are each one saved version.

#[path = "fixtures/tasks.rs"]
mod fixture;
#[path = "fixtures/server.rs"]
mod server;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fixture::{TempFile, NAME, TABLE};
use jiff::civil::{date, Time};
use jiff::tz::TimeZone;
use jiff::Zoned;
use om_hbam::slot::uint_segment;
use om_hbam::{serial, HbamFile, OwnedNode};
use om_protocol::write;
use om_server::{
    BackupError, BackupFiles, BackupTask, Days, ManualClock, Repeat, Schedule, ScheduleError,
    ScheduleTask, ScriptTask, ServerHandle, Times,
};
use om_session::Session;

const WAIT: Duration = Duration::from_secs(20);

/// The marker a run of this server writes in an unfinished backup, as
/// process `pid` started at `started`.
fn marker(pid: u32, started: u64) -> String {
    format!(
        "OpenMaker Server is writing this backup. If it stops first, its next run of the \
         schedule removes this folder.\npid={pid}\nstarted={started}\n"
    )
}

/// 2026-10-29 at `h:m` UTC.
fn at(h: i8, m: i8) -> Zoned {
    date(2026, 10, 29)
        .at(h, m, 0, 0)
        .to_zoned(TimeZone::UTC)
        .unwrap()
}

/// A folder of the test's own, removed when dropped.
struct Folder(PathBuf);

impl Folder {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("om-backups-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Folder(path)
    }
}

impl Drop for Folder {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The fixture file as `<folder>/db/Sample.fmp12`.
fn hosted_file(folder: &Folder) -> TempFile {
    let built = TempFile::new("backup", &[], true);
    let db = folder.0.join("db");
    std::fs::create_dir_all(&db).unwrap();
    let path = db.join("Sample.fmp12");
    std::fs::rename(&built.0, &path).unwrap();
    TempFile(path)
}

/// A backup schedule of every file keeping `keep`, every minute from 9:00
/// to 10:00.
fn backup_schedule(name: &str, keep: u8) -> Schedule {
    backup_schedule_at(
        name,
        keep,
        Times::Every {
            interval: Duration::from_secs(60),
            from: Time::new(9, 0, 0, 0).unwrap(),
            until: Time::new(10, 0, 0, 0).unwrap(),
        },
    )
}

/// A backup schedule of every file keeping `keep`, daily at `times`.
fn backup_schedule_at(name: &str, keep: u8, times: Times) -> Schedule {
    Schedule {
        name: name.into(),
        task: ScheduleTask::Backup(BackupTask {
            files: BackupFiles::All,
            folder: None,
            keep,
        }),
        repeat: Repeat::Days {
            days: Days::Daily,
            times,
            start: None,
            end: None,
        },
        enabled: true,
    }
}

/// The last run of schedule `name`.
fn last_run(handle: &ServerHandle, name: &str) -> om_server::LastRun {
    handle
        .schedules()
        .into_iter()
        .find(|s| s.schedule.name == name)
        .and_then(|s| s.last_run)
        .unwrap()
}

/// A server hosting `file`, backing up to `backups` with `schedules` on
/// `clock`, serving; its port and handle.
fn start(
    file: &TempFile,
    backups: &Path,
    schedules: Vec<Schedule>,
    clock: &Arc<ManualClock>,
) -> (u16, ServerHandle) {
    let (port, handle) = server::start_configured_with_handle(file, |server| {
        server
            .with_clock(clock.clone())
            .with_backup_folder(backups)
            .unwrap()
            .with_schedules(schedules)
            .unwrap()
    });
    assert!(
        handle.wait_for_schedules(WAIT),
        "the scheduler did not start"
    );
    (port, handle)
}

/// Move `clock` and let the scheduler start and finish what is due.
fn move_to(handle: &ServerHandle, clock: &ManualClock, now: Zoned) {
    clock.set(now);
    assert!(handle.wait_for_schedules(WAIT), "runs did not end");
}

/// Back up now with `schedule` and wait for the run; its folder name.
fn back_up_now(handle: &ServerHandle, schedule: &str) -> String {
    handle.back_up_now(schedule).unwrap();
    assert!(handle.wait_for_schedules(WAIT), "the backup did not end");
    let last = last_run(handle, schedule);
    assert!(last.succeeded() && last.manual, "{last:?}");
    last.result
}

/// The names in `folder`, sorted.
fn names(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// Record `record`'s Name in the file at `path`, read with om-hbam.
fn stored_name(path: &Path, record: u64) -> Option<String> {
    HbamFile::open(path)
        .unwrap()
        .cell(TABLE, record, NAME)
        .unwrap()
        .and_then(|cell| cell.text().and_then(Result::ok))
}

/// Lock record 1 of Tasks on `session`, commit `name` into it and let it
/// go: one save of the file.
fn commit_name(session: &mut Session, name: &str) {
    let (table, record) = (uint_segment(TABLE), serial::encode(1));
    let path: [&[u8]; 3] = [&table, &[5], &record];
    assert!(matches!(
        session.lock(&path, 0x31, true).unwrap(),
        write::LockReply::Granted { .. }
    ));
    let stored = OwnedNode::list(
        record.clone(),
        vec![
            OwnedNode::leaf(uint_segment(NAME), om_scsu::encode_scsu(name)),
            OwnedNode::leaf(vec![0xfc], vec![0x01, 0x01]),
        ],
    );
    let changed = OwnedNode::list(
        record,
        vec![OwnedNode::leaf(uint_segment(NAME), Vec::new())],
    );
    session.upload_records(&table, vec![stored]).unwrap();
    session.commit_records(&table, vec![changed], 0x31).unwrap();
    session.kill_all_locks(0x31).unwrap();
}

#[cfg(unix)]
fn inode(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).unwrap().ino()
}

#[test]
fn a_backup_is_filemakers_folder_of_copies_that_open_on_a_server() {
    let folder = Folder::new("copies");
    let file = hosted_file(&folder);
    // Externally stored container data beside the file, with a link that
    // is not followed.
    let containers = folder.0.join("db/RC_Data_FMS/Sample/Photo");
    std::fs::create_dir_all(&containers).unwrap();
    std::fs::write(containers.join("a.png"), b"png").unwrap();
    std::fs::write(folder.0.join("secret"), b"secret").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(folder.0.join("secret"), containers.join("link")).unwrap();
    let backups = folder.0.join("Backups");
    let clock = ManualClock::new(at(8, 59));
    let nightly = backup_schedule_at("Nightly", 7, Times::At(Time::new(9, 0, 0, 0).unwrap()));
    let script = Schedule {
        name: "Import".into(),
        task: ScheduleTask::Script(ScriptTask {
            file: fixture::FILE.into(),
            script: "Import".into(),
            account: fixture::ACCOUNT.into(),
            parameter: String::new(),
            time_limit: None,
            end_on_time_limit: false,
        }),
        enabled: false,
        ..nightly.clone()
    };
    let (port, handle) = start(&file, &backups, vec![nightly, script], &clock);
    assert_eq!(handle.backup_folder().as_deref(), Some(backups.as_path()));
    assert!(handle.backups().is_empty());

    // The run at 9:00, on its own.
    move_to(&handle, &clock, at(9, 0));
    assert_eq!(names(&backups), ["Nightly_2026-10-29_0900"]);
    let first = backups.join("Nightly_2026-10-29_0900");
    assert_eq!(names(&first), ["Databases", "backup.config"]);
    let config = std::fs::read_to_string(first.join("backup.config")).unwrap();
    let id = config
        .strip_prefix("Label=Auto-backup\nTimeStamp=2026-10-29 09:00:00\nIsPreserved=false\nID=")
        .and_then(|id| id.strip_suffix('\n'))
        .unwrap_or_else(|| panic!("{config:?}"));
    assert!(id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()));
    let copy = first.join("Databases/Sample.fmp12");
    assert_eq!(
        std::fs::read(&copy).unwrap(),
        std::fs::read(&file.0).unwrap()
    );
    assert_eq!(stored_name(&copy, 1).as_deref(), Some("Write tests"));
    assert_eq!(
        std::fs::read(first.join("Databases/RC_Data_FMS/Sample/Photo/a.png")).unwrap(),
        b"png"
    );
    assert_eq!(
        names(&first.join("Databases/RC_Data_FMS/Sample/Photo")),
        ["a.png"]
    );
    let listed = handle.backups();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        (
            listed[0].id.as_str(),
            listed[0].schedule.as_str(),
            listed[0].folder.as_str(),
            listed[0].label.as_str(),
            listed[0].preserved,
        ),
        (
            id,
            "Nightly",
            "Nightly_2026-10-29_0900",
            "Auto-backup",
            false
        )
    );
    assert_eq!(listed[0].started, date(2026, 10, 29).at(9, 0, 0, 0));
    assert_eq!(listed[0].time, Some(at(9, 0).timestamp()));
    assert_eq!(
        listed[0].size,
        std::fs::metadata(&copy).unwrap().len() + 3 + config.len() as u64
    );

    // The copy opens on a server (a copy of it: a server writes what it
    // hosts).
    let rehosted = TempFile(folder.0.join("Rehosted.fmp12"));
    std::fs::copy(&copy, &rehosted.0).unwrap();
    let rehosted_port = server::start(&rehosted);
    let (mut other, _) = server::logged_in(rehosted_port, "restorer");
    let records = server::records_of(&mut other, TABLE);
    assert_eq!(server::cell(&records, 1, NAME), Some("Write tests"));
    assert_eq!(server::cell(&records, 2, NAME), Some("Ship"));

    // A change, then Back Up Now: a new copy of the new version.
    let (mut client, _) = server::logged_in(port, "writer");
    commit_name(&mut client, "Changed");
    clock.set(at(9, 0) + Duration::from_secs(30));
    handle.back_up_now("Nightly").unwrap();
    assert!(handle.wait_for_schedules(WAIT));
    // The minute's folder exists: the run fails and leaves nothing.
    let failed = last_run(&handle, "Nightly");
    assert!(!failed.succeeded() && failed.error == 800, "{failed:?}");
    assert_eq!(names(&backups), ["Nightly_2026-10-29_0900"]);
    clock.set(at(9, 0) + Duration::from_secs(70));
    let second = backups.join(back_up_now(&handle, "Nightly"));
    assert_eq!(second, backups.join("Nightly_2026-10-29_0901"));
    let second_copy = second.join("Databases/Sample.fmp12");
    assert_eq!(stored_name(&second_copy, 1).as_deref(), Some("Changed"));
    assert_eq!(
        std::fs::read(&second_copy).unwrap(),
        std::fs::read(&file.0).unwrap()
    );
    assert!(std::fs::read_to_string(second.join("backup.config"))
        .unwrap()
        .starts_with("Label=On-demand\nTimeStamp=2026-10-29 09:01:10\n"));
    // Nothing changed since: the next backup links that copy.
    clock.set(at(9, 2));
    let third = backups.join(back_up_now(&handle, "Nightly"));
    let third_copy = third.join("Databases/Sample.fmp12");
    assert_eq!(
        std::fs::read(&third_copy).unwrap(),
        std::fs::read(&file.0).unwrap()
    );
    #[cfg(unix)]
    {
        assert_ne!(inode(&copy), inode(&second_copy));
        assert_eq!(inode(&second_copy), inode(&third_copy));
    }
    assert!(!names(&third.join("Databases/RC_Data_FMS/Sample/Photo")).contains(&"link".into()));
    assert_eq!(
        handle.back_up_now("Import"),
        Err(ScheduleError::NotBackup("Import".into()))
    );
    assert_eq!(
        handle.back_up_now("Missing"),
        Err(ScheduleError::NotFound("Missing".into()))
    );
    assert!(handle.running_backups().is_empty());
    assert!(!handle.cancel_backup());
}

#[test]
fn retention_keeps_the_newest_and_the_preserved_and_nothing_else() {
    let folder = Folder::new("retention");
    let file = hosted_file(&folder);
    let backups = folder.0.join("Backups");
    std::fs::create_dir_all(&backups).unwrap();
    // Not this schedule's backups, nor backups at all: kept. Unfinished
    // backups of the schedule: removed by its next run when this server's
    // marker names a writer that is gone, else left alone.
    let outside = folder.0.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(
        outside.join("backup.config"),
        "Label=x\nTimeStamp=2026-10-28 09:00:00\nIsPreserved=false\n\
         ID=00000000000000000000000000000001\n",
    )
    .unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, backups.join("Every_2026-10-28_0900")).unwrap();
    std::fs::create_dir_all(backups.join("Other_2026-10-28_0900")).unwrap();
    std::fs::copy(
        outside.join("backup.config"),
        backups.join("Other_2026-10-28_0900/backup.config"),
    )
    .unwrap();
    std::fs::create_dir_all(backups.join("Every_2026-10-28_0901/Databases")).unwrap();
    // A process that does not run: pid 2^31 - 1 is past every system's
    // pid_max.
    let gone = marker(i32::MAX as u32, 1);
    for (minute, text) in [
        ("0902", None),
        ("0903", Some(gone.clone())),
        ("0904", Some("pid=one\n".to_string())),
        // Only exactly the marker: not one with more after it, nor a
        // large file that begins with one.
        ("0905", Some(format!("{gone}more\n"))),
        ("0906", Some(format!("{gone}{}", " ".repeat(1 << 20)))),
    ] {
        let unfinished = backups.join(format!("Every_2026-10-28_{minute}_InProgress"));
        std::fs::create_dir_all(unfinished.join("Databases")).unwrap();
        if let Some(text) = text {
            std::fs::write(unfinished.join(".openmaker-in-progress"), text).unwrap();
        }
    }
    let clock = ManualClock::new(at(8, 59));
    let (_, handle) = start(&file, &backups, vec![backup_schedule("Every", 2)], &clock);

    move_to(&handle, &clock, at(9, 0));
    let first = handle
        .backups()
        .into_iter()
        .find(|b| b.folder == "Every_2026-10-29_0900")
        .unwrap();
    assert_eq!(
        handle.delete_backup(&first.id),
        Err(BackupError::NotPreserved(first.id.clone()))
    );
    let preserved = handle
        .preserve_backup(&first.id.to_lowercase(), true)
        .unwrap();
    assert!(preserved.preserved);
    let labeled = handle
        .set_backup_label(&first.id, "Before the upgrade")
        .unwrap();
    assert_eq!(
        (labeled.label.as_str(), labeled.preserved),
        ("Before the upgrade", true)
    );
    assert!(matches!(
        handle.set_backup_label(&first.id, "two\nlines"),
        Err(BackupError::InvalidLabel(_))
    ));
    for minute in 1..=3 {
        move_to(&handle, &clock, at(9, minute));
    }
    let mut expected = vec![
        "Every_2026-10-28_0901",
        "Every_2026-10-28_0902_InProgress",
        "Every_2026-10-28_0904_InProgress",
        "Every_2026-10-28_0905_InProgress",
        "Every_2026-10-28_0906_InProgress",
        "Every_2026-10-29_0900",
        "Every_2026-10-29_0902",
        "Every_2026-10-29_0903",
        "Other_2026-10-28_0900",
    ];
    #[cfg(unix)]
    expected.insert(0, "Every_2026-10-28_0900");
    assert_eq!(names(&backups), expected);
    assert!(outside.join("backup.config").exists());
    let config =
        std::fs::read_to_string(backups.join("Every_2026-10-29_0900/backup.config")).unwrap();
    assert!(
        config.starts_with(
            "Label=Before the upgrade\nTimeStamp=2026-10-29 09:00:00\nIsPreserved=true\n"
        ),
        "{config:?}"
    );
    // Listed: this server's backups and the other schedule's, newest
    // first; not the link, nor a folder without a backup.config.
    let listed: Vec<(String, bool)> = handle
        .backups()
        .into_iter()
        .map(|b| (b.folder, b.preserved))
        .collect();
    assert_eq!(
        listed,
        [
            ("Every_2026-10-29_0903".to_string(), false),
            ("Every_2026-10-29_0902".to_string(), false),
            ("Every_2026-10-29_0900".to_string(), true),
            ("Other_2026-10-28_0900".to_string(), false),
        ]
    );

    // No longer preserved, it is the oldest at the next run.
    handle.preserve_backup(&first.id, false).unwrap();
    move_to(&handle, &clock, at(9, 4));
    assert!(!backups.join("Every_2026-10-29_0900").exists());
    assert!(!backups.join("Every_2026-10-29_0902").exists());
    // A preserved backup is deleted by hand.
    let newest = handle.backups()[0].clone();
    assert_eq!(newest.folder, "Every_2026-10-29_0904");
    handle.preserve_backup(&newest.id, true).unwrap();
    handle.delete_backup(&newest.id).unwrap();
    assert!(!newest.path.exists());
    assert_eq!(
        handle.delete_backup(&newest.id),
        Err(BackupError::NotFound(newest.id.clone()))
    );
}

/// A database whose file name is as long as a name can be (255 bytes) is
/// backed up under that name: the copy is made without a name of its own
/// and linked to it once complete, so no longer name is ever needed.
#[test]
fn a_file_with_the_longest_name_is_backed_up() {
    let folder = Folder::new("longest-name");
    let built = hosted_file(&folder);
    let longest = built
        .0
        .with_file_name(format!("{}.fmp12", "L".repeat(255 - ".fmp12".len())));
    std::fs::rename(&built.0, &longest).unwrap();
    let file = TempFile(longest);
    drop(built);
    let backups = folder.0.join("Backups");
    let clock = ManualClock::new(at(8, 0));
    let mut schedule = backup_schedule("Long", 3);
    schedule.enabled = false;
    let (_, handle) = start(&file, &backups, vec![schedule], &clock);
    clock.set(at(8, 1));
    let made = back_up_now(&handle, "Long");
    let copy = backups
        .join(made)
        .join("Databases")
        .join(file.0.file_name().unwrap());
    assert_eq!(file.0.file_name().unwrap().len(), 255);
    assert_eq!(
        std::fs::read(&copy).unwrap(),
        std::fs::read(&file.0).unwrap()
    );
}

/// A backup folder on a file system without hard links (simulated: the
/// server's test switch makes every link to it fail as such a file system
/// fails it) gets a full copy of each file under its name in every backup,
/// as before exports: none is linked to an earlier backup's copy.
#[test]
fn a_folder_without_hard_links_gets_full_copies() {
    let folder = Folder::new("no-hard-links");
    let file = hosted_file(&folder);
    let backups = folder.0.join("Backups");
    let clock = ManualClock::new(at(8, 0));
    let mut schedule = backup_schedule("Copies", 5);
    schedule.enabled = false;
    let (_, handle) = start(&file, &backups, vec![schedule], &clock);
    handle.backing_up_without_hard_links();
    clock.set(at(8, 1));
    let first = back_up_now(&handle, "Copies");
    // Nothing changed: on a file system with links this one is linked.
    clock.set(at(8, 2));
    let second = back_up_now(&handle, "Copies");
    let copy = |made: &str| backups.join(made).join("Databases/Sample.fmp12");
    for made in [&first, &second] {
        assert_eq!(
            std::fs::read(copy(made)).unwrap(),
            std::fs::read(&file.0).unwrap()
        );
    }
    #[cfg(unix)]
    assert_ne!(inode(&copy(&first)), inode(&copy(&second)));
}

/// A file closed, replaced and opened again keeps its id: the next backup
/// copies the file it holds now, never links the copy of the one it held
/// before, on either engine (the file engine's save count starts over at
/// each load; the store engine's never moves).
#[cfg(unix)]
#[test]
fn a_backup_after_a_file_was_replaced_and_opened_again_copies_the_new_file() {
    for engine in ["file", "store"] {
        let folder = Folder::new(&format!("replaced-{engine}"));
        let file = hosted_file(&folder);
        let backups = folder.0.join("Backups");
        let store = folder.0.join("Store");
        let clock = ManualClock::new(at(8, 0));
        let mut schedule = backup_schedule("Copies", 5);
        schedule.enabled = false;
        let configure = {
            let (clock, backups) = (clock.clone(), backups.clone());
            move |server: om_server::Server| {
                server
                    .with_clock(clock)
                    .with_backup_folder(&backups)
                    .unwrap()
                    .with_schedules(vec![schedule])
                    .unwrap()
            }
        };
        let (_, handle) = match engine {
            "file" => server::start_configured_with_handle(&file, configure),
            _ => server::start_on_store(&[(fixture::FILE, &file)], &store, configure),
        };
        assert!(handle.wait_for_schedules(WAIT), "{engine}");
        clock.set(at(8, 1));
        let first = back_up_now(&handle, "Copies");
        close_file(&handle, 1);
        let mut root = fixture::tree(&[], true);
        let path = [
            &uint_segment(TABLE)[..],
            &[5],
            &serial::encode(1),
            &uint_segment(NAME),
        ];
        root.put(&path, om_scripts::text_node(&[], "Replaced"));
        fixture::write_tree(&file.0, &root);
        handle.open_file(1).unwrap();
        clock.set(at(8, 2));
        let second = back_up_now(&handle, "Copies");
        let copy = |made: &str| backups.join(made).join("Databases/Sample.fmp12");
        assert_eq!(
            stored_name(&copy(&first), 1).as_deref(),
            Some("Write tests"),
            "{engine}"
        );
        assert_eq!(
            stored_name(&copy(&second), 1).as_deref(),
            Some("Replaced"),
            "{engine}"
        );
        assert_ne!(inode(&copy(&first)), inode(&copy(&second)), "{engine}");
    }
}

#[test]
fn backups_taken_while_a_client_writes_are_each_a_saved_version() {
    let folder = Folder::new("concurrent");
    let file = hosted_file(&folder);
    let backups = folder.0.join("Backups");
    let clock = ManualClock::new(at(8, 0));
    let mut busy = backup_schedule("Busy", 99);
    // Only on demand.
    busy.enabled = false;
    let (port, handle) = start(&file, &backups, vec![busy], &clock);
    // Each commit is one save: the writer reads the file after each, so
    // every version the file had is in `versions`.
    let versions = Arc::new(Mutex::new(vec![std::fs::read(&file.0).unwrap()]));
    let writer = {
        let versions = versions.clone();
        let path = file.0.clone();
        let (mut client, _) = server::logged_in(port, "writer");
        std::thread::spawn(move || {
            for i in 0..40 {
                commit_name(&mut client, &format!("Version {i}"));
                versions.lock().unwrap().push(std::fs::read(&path).unwrap());
            }
        })
    };
    let mut made = Vec::new();
    let mut minute = 0;
    while !writer.is_finished() || made.len() < 3 {
        minute += 1;
        clock.set(at(8, 0) + Duration::from_secs(60 * minute));
        made.push(back_up_now(&handle, "Busy"));
    }
    writer.join().unwrap();
    let versions = versions.lock().unwrap();
    assert_eq!(versions.len(), 41);
    for name in &made {
        let copy = backups.join(name).join("Databases/Sample.fmp12");
        let bytes = std::fs::read(&copy).unwrap();
        assert!(versions.contains(&bytes), "{name} is not a saved version");
        assert!(stored_name(&copy, 1).is_some());
    }
}

/// A server for `file` with `schedules`, not serving: to see what it
/// refuses.
fn server_for(file: &Path) -> om_server::Server {
    let config = om_server::ServerConfig {
        identity: om_server::ServerIdentity {
            name: "openmaker-test".into(),
            version_string: "OpenMaker Server test".into(),
            server_id: "0123456789abcdef0123456789abcdef".into(),
            web_service_settings: String::new(),
        },
        files: vec![om_server::HostedFile {
            name: fixture::FILE.into(),
            path: file.to_path_buf(),
        }],
        insecure_files: false,
        record_dir: None,
    };
    let identity = om_server::Identity::self_signed(vec!["localhost".into()]).unwrap();
    let server = om_server::Server::bind("127.0.0.1:0", config, identity).unwrap();
    // Reads through views are answered through the test view too, and
    // checked against the file engine's (docs/server-storage.md 5, "Views").
    #[cfg(debug_assertions)]
    let server = server.checking_reads();
    server
}

/// A finished backup of schedule `name` at `minute` on 2026-10-28, as
/// this server writes one, in `backups`.
fn plant_backup(backups: &Path, name: &str, minute: &str) -> PathBuf {
    let folder = backups.join(format!("{name}_2026-10-28_{minute}"));
    std::fs::create_dir_all(folder.join("Databases")).unwrap();
    std::fs::write(
        folder.join("backup.config"),
        format!(
            "Label=Auto-backup\nTimeStamp=2026-10-28 09:{}:00\nIsPreserved=false\n\
             ID=0000000000000000000000000000{minute}\n",
            &minute[2..]
        ),
    )
    .unwrap();
    folder
}

#[cfg(unix)]
fn close_file(handle: &ServerHandle, id: u32) {
    handle
        .close_file(
            id,
            &om_server::Notice {
                message: String::new(),
                grace: Duration::ZERO,
            },
        )
        .unwrap();
    let deadline = std::time::Instant::now() + WAIT;
    while !handle
        .files()
        .iter()
        .any(|file| file.id == id && file.status == om_server::FileStatus::Closed)
    {
        assert!(std::time::Instant::now() < deadline, "file did not close");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Retention keeps the newest backup of a schedule that holds a hosted
/// file no newer backup that stays holds: a run that left a closed file out
/// does not retire the last backup of it (#1115). Once the file is copied
/// again, the old backup goes at the next run; and a backup that holds only
/// files the server no longer hosts goes as before.
#[cfg(unix)]
#[test]
fn retention_keeps_the_last_backup_of_a_closed_file() {
    let folder = Folder::new("retention-closed");
    let file = hosted_file(&folder);
    let elsewhere = folder.0.join("other");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let built = TempFile::new("backup-closed-other", &[], true);
    let other = TempFile(elsewhere.join("Other.fmp12"));
    std::fs::rename(&built.0, &other.0).unwrap();
    let backups = folder.0.join("Backups");
    let gone = plant_backup(&backups, "Every", "0800");
    std::fs::create_dir_all(gone.join("Databases/gone")).unwrap();
    std::fs::write(gone.join("Databases/gone/Gone.fmp12"), b"not hosted").unwrap();

    let clock = ManualClock::new(at(8, 59));
    let (_, handle) =
        server::start_hosting(&[(fixture::FILE, &file), ("Other", &other)], |server| {
            server
                .with_clock(clock.clone())
                .with_backup_folder(&backups)
                .unwrap()
                .with_schedules(vec![backup_schedule("Every", 1)])
                .unwrap()
        });
    assert!(
        handle.wait_for_schedules(WAIT),
        "the scheduler did not start"
    );
    let other_id = handle
        .files()
        .iter()
        .find(|info| info.name == "Other")
        .unwrap()
        .id;
    let both = backups.join("Every_2026-10-29_0900");

    // Both files copied; the planted backup holds only a file no longer
    // hosted, and goes as before.
    move_to(&handle, &clock, at(9, 0));
    assert!(both.join("Databases/other/Other.fmp12").exists());
    assert!(
        !gone.exists(),
        "a backup of files no longer hosted is deleted"
    );

    // Other closed: the run copies Sample alone, and the backup that holds
    // Other stays past the keep count.
    close_file(&handle, other_id);
    move_to(&handle, &clock, at(9, 1));
    let alone = backups.join("Every_2026-10-29_0901");
    assert!(alone.join("Databases/db/Sample.fmp12").exists());
    assert!(!alone.join("Databases/other").exists());
    assert!(both.exists(), "the last backup of a closed file is kept");

    // Other open again and copied: both older backups go.
    handle.open_file(other_id).unwrap();
    move_to(&handle, &clock, at(9, 2));
    let again = backups.join("Every_2026-10-29_0902");
    assert!(again.join("Databases/other/Other.fmp12").exists());
    assert!(!alone.exists(), "{alone:?} kept");
    assert!(!both.exists(), "{both:?} kept once Other was copied again");
}

#[cfg(unix)]
#[test]
fn a_backup_folder_never_holds_a_hosted_file() {
    let folder = Folder::new("overlap");
    let backups = folder.0.join("Backups");
    // A hosted file inside an older backup of the schedule: refused as
    // the default folder and as a schedule's own.
    let old = plant_backup(&backups, "Nightly", "0900");
    let built = TempFile::new("overlap", &[], true);
    let inside = old.join("Databases/Sample.fmp12");
    std::fs::rename(&built.0, &inside).unwrap();
    let refused = server_for(&inside).with_backup_folder(&backups);
    assert!(
        matches!(refused, Err(ScheduleError::BackupFolder { .. })),
        "{:?}",
        refused.err()
    );
    // Closing a file stops serving it, but does not make its folder
    // eligible for backup retention or deletion.
    {
        let closed = server_for(&inside);
        close_file(&closed.handle().unwrap(), 1);
        assert!(matches!(
            closed.with_backup_folder(&backups),
            Err(ScheduleError::BackupFolder { .. })
        ));
        assert!(inside.exists());
    }
    let mut own = backup_schedule("Nightly", 1);
    if let ScheduleTask::Backup(task) = &mut own.task {
        task.folder = Some(backups.clone());
    }
    let refused = server_for(&inside).with_schedules(vec![own]);
    assert!(
        matches!(refused, Err(ScheduleError::Invalid { .. })),
        "{:?}",
        refused.err()
    );
    // Inside a hosted file's container data: refused. Beside the hosted
    // files, or below their folder: fine.
    let db = folder.0.join("db");
    std::fs::create_dir_all(&db).unwrap();
    let hosted = db.join("Sample.fmp12");
    std::fs::rename(&inside, &hosted).unwrap();
    assert!(server_for(&hosted)
        .with_backup_folder(db.join("RC_Data_FMS/Backups"))
        .is_err());
    assert!(server_for(&hosted).with_backup_folder(&db).is_err());
    assert!(server_for(&hosted).with_backup_folder(&backups).is_ok());
    assert!(server_for(&hosted)
        .with_backup_folder(db.join("Backups"))
        .is_ok());

    // A hosted file that comes to lie in an old backup after the server
    // started (its folder is a link the administrator moved): retention
    // keeps that backup, and the run still completes.
    let link = folder.0.join("link");
    std::os::unix::fs::symlink(&db, &link).unwrap();
    let file = TempFile(link.join("Sample.fmp12"));
    let clock = ManualClock::new(at(8, 59));
    let (_, handle) = start(&file, &backups, vec![backup_schedule("Nightly", 1)], &clock);
    std::fs::rename(&hosted, &inside).unwrap();
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(old.join("Databases"), &link).unwrap();
    move_to(&handle, &clock, at(9, 0));
    assert!(last_run(&handle, "Nightly").succeeded());
    assert_eq!(
        names(&backups),
        ["Nightly_2026-10-28_0900", "Nightly_2026-10-29_0900"]
    );
    assert!(inside.exists());
    // Nor does Delete remove it.
    let planted = handle
        .backups()
        .into_iter()
        .find(|b| b.folder == "Nightly_2026-10-28_0900")
        .unwrap();
    handle.preserve_backup(&planted.id, true).unwrap();
    assert!(matches!(
        handle.delete_backup(&planted.id),
        Err(BackupError::Io(_))
    ));
    assert!(inside.exists());

    // Nor does the cleanup of an unfinished backup, abandoned by its
    // marker, remove one a hosted file has come to lie in.
    let unfinished = backups.join("Nightly_2026-10-29_0800_InProgress");
    std::fs::create_dir_all(unfinished.join("Databases")).unwrap();
    std::fs::write(
        unfinished.join(".openmaker-in-progress"),
        marker(i32::MAX as u32, 1),
    )
    .unwrap();
    std::fs::write(unfinished.join("unrelated"), b"kept").unwrap();
    let moved = unfinished.join("Databases/Sample.fmp12");
    std::fs::rename(&inside, &moved).unwrap();
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(unfinished.join("Databases"), &link).unwrap();
    move_to(&handle, &clock, at(9, 1));
    assert!(last_run(&handle, "Nightly").succeeded());
    assert!(moved.exists() && unfinished.join("unrelated").exists());
}

#[cfg(unix)]
#[test]
fn a_hosted_file_reached_through_a_file_link_is_never_deleted() {
    let folder = Folder::new("file-link");
    let backups = folder.0.join("Backups");
    let preserved = plant_backup(&backups, "Every", "0900");
    let config = std::fs::read_to_string(preserved.join("backup.config")).unwrap();
    std::fs::write(
        preserved.join("backup.config"),
        config.replace("IsPreserved=false", "IsPreserved=true"),
    )
    .unwrap();
    let inside = preserved.join("Databases/Sample.fmp12");
    let built = TempFile::new("file-link", &[], true);
    std::fs::rename(&built.0, &inside).unwrap();
    let db = folder.0.join("db");
    std::fs::create_dir_all(&db).unwrap();
    // The hosted path is a link to a file in a preserved backup: refused.
    let hosted = db.join("Sample.fmp12");
    std::os::unix::fs::symlink(&inside, &hosted).unwrap();
    assert!(server_for(&hosted).with_backup_folder(&backups).is_err());

    // A saved file moved into a backup after startup stays protected even
    // if the administrator points the configured link at another file.
    let real = folder.0.join("real");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::rename(&inside, real.join("Sample.fmp12")).unwrap();
    std::fs::remove_file(&hosted).unwrap();
    std::os::unix::fs::symlink(real.join("Sample.fmp12"), &hosted).unwrap();
    let file = TempFile(hosted.clone());
    let clock = ManualClock::new(at(8, 0));
    let (_, handle) = start(&file, &backups, vec![], &clock);
    let moved = preserved.join("Live");
    std::fs::rename(&real, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &real).unwrap();
    let replacement = db.join("Other.fmp12");
    let saved = moved.join("Sample.fmp12");
    std::fs::copy(&saved, &replacement).unwrap();
    std::fs::remove_file(&hosted).unwrap();
    std::os::unix::fs::symlink(&replacement, &hosted).unwrap();
    let id = handle.backups()[0].id.clone();
    assert!(matches!(handle.delete_backup(&id), Err(BackupError::Io(_))));
    assert!(saved.exists());
    close_file(&handle, 1);
    assert!(matches!(handle.delete_backup(&id), Err(BackupError::Io(_))));
    assert!(saved.exists());
    handle.open_file(1).unwrap();
}

#[cfg(unix)]
#[test]
fn an_unresolved_save_location_blocks_retention_but_not_an_unrelated_backup() {
    let folder = Folder::new("unresolved-save-location");
    let file = hosted_file(&folder);
    let linked = TempFile::new("linked-backup", &[], true);
    let real = folder.0.join("Real");
    std::fs::create_dir(&real).unwrap();
    let saved = real.join("Linked.fmp12");
    std::fs::rename(&linked.0, &saved).unwrap();
    let alias = TempFile(folder.0.join("Alias.fmp12"));
    std::os::unix::fs::symlink(&saved, &alias.0).unwrap();

    let backups = folder.0.join("Backups");
    let old = plant_backup(&backups, "Nightly", "0900");
    let clock = ManualClock::new(at(8, 59));
    let mut schedule = backup_schedule("Nightly", 1);
    let ScheduleTask::Backup(task) = &mut schedule.task else {
        unreachable!();
    };
    task.files = BackupFiles::File(fixture::FILE.into());
    let (_, handle) =
        server::start_hosting(&[(fixture::FILE, &file), ("Alias", &alias)], |server| {
            server
                .with_clock(clock.clone())
                .with_backup_folder(&backups)
                .unwrap()
                .with_schedules(vec![schedule])
                .unwrap()
        });
    assert!(
        handle.wait_for_schedules(WAIT),
        "the scheduler did not start"
    );
    // A symlink loop gives the secondary container lookup an I/O error
    // even when the test runner can bypass file permissions.
    std::os::unix::fs::symlink("RC_Data_FMS", real.join("RC_Data_FMS")).unwrap();
    move_to(&handle, &clock, at(9, 0));
    let run = last_run(&handle, "Nightly");
    assert!(run.succeeded(), "{run:?}");
    assert_eq!(
        std::fs::read(backups.join(run.result).join("Databases/db/Sample.fmp12")).unwrap(),
        std::fs::read(&file.0).unwrap(),
    );
    assert!(old.exists(), "unresolved data prevents retention");
}

/// A backup of one folder copies the hosted files in it and no other
/// (`BackupFiles::Folder`, whose folders are all resolved before any file is
/// taken, #1006).
#[test]
fn a_folder_backup_copies_the_files_in_its_folder_alone() {
    let folder = Folder::new("one-folder");
    let file = hosted_file(&folder);
    let elsewhere = folder.0.join("other");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let built = TempFile::new("backup-other", &[], true);
    let other = TempFile(elsewhere.join("Other.fmp12"));
    std::fs::rename(&built.0, &other.0).unwrap();

    let backups = folder.0.join("Backups");
    let clock = ManualClock::new(at(8, 59));
    let mut schedule = backup_schedule("Folder", 1);
    let ScheduleTask::Backup(task) = &mut schedule.task else {
        unreachable!();
    };
    task.files = BackupFiles::Folder(folder.0.join("db"));
    let (_, handle) =
        server::start_hosting(&[(fixture::FILE, &file), ("Other", &other)], |server| {
            server
                .with_clock(clock.clone())
                .with_backup_folder(&backups)
                .unwrap()
                .with_schedules(vec![schedule])
                .unwrap()
        });
    assert!(
        handle.wait_for_schedules(WAIT),
        "the scheduler did not start"
    );
    let databases = backups
        .join(back_up_now(&handle, "Folder"))
        .join("Databases");
    assert_eq!(
        std::fs::read(databases.join("db/Sample.fmp12")).unwrap(),
        std::fs::read(&file.0).unwrap(),
    );
    assert!(!databases.join("other").exists(), "{databases:?}");
}

#[cfg(unix)]
#[test]
fn container_data_is_never_a_backup_folder() {
    let folder = Folder::new("containers");
    let file = hosted_file(&folder);
    let own = folder.0.join("db/RC_Data_FMS/Sample");
    std::fs::create_dir_all(&own).unwrap();
    std::fs::write(own.join("asset.png"), b"png").unwrap();
    // A folder of container data that looks like a preserved backup.
    let lookalike = own.join("Every_2026-10-29_0900");
    std::fs::create_dir_all(&lookalike).unwrap();
    std::fs::write(lookalike.join("photo.png"), b"photo").unwrap();
    std::fs::write(
        lookalike.join("backup.config"),
        "Label=x\nTimeStamp=2026-10-29 09:00:00\nIsPreserved=true\n\
         ID=0000000000000000000000000000ABCD\n",
    )
    .unwrap();

    // Refused at startup inside container data that a link leads to.
    let elsewhere = folder.0.join("elsewhere");
    let db2 = folder.0.join("db2");
    std::fs::create_dir_all(elsewhere.join("Backups")).unwrap();
    std::fs::create_dir_all(&db2).unwrap();
    std::fs::copy(&file.0, db2.join("Sample.fmp12")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, db2.join("RC_Data_FMS")).unwrap();
    for target in [elsewhere.join("Backups"), elsewhere.clone()] {
        assert!(
            server_for(&db2.join("Sample.fmp12"))
                .with_backup_folder(&target)
                .is_err(),
            "{}",
            target.display()
        );
    }

    // A target whose link the administrator moves into container data
    // after startup is not used: Delete finds nothing there.
    let backups = folder.0.join("Backups");
    std::fs::create_dir_all(&backups).unwrap();
    let link = folder.0.join("target");
    std::os::unix::fs::symlink(&backups, &link).unwrap();
    let clock = ManualClock::new(at(8, 0));
    let (_, handle) = start(&file, &link, vec![], &clock);
    for into in [&own, &folder.0.join("db/RC_Data_FMS")] {
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(into, &link).unwrap();
        assert!(handle.backups().is_empty());
        assert_eq!(
            handle.delete_backup("0000000000000000000000000000ABCD"),
            Err(BackupError::NotFound(
                "0000000000000000000000000000ABCD".into()
            ))
        );
    }
    assert!(lookalike.join("photo.png").exists() && own.join("asset.png").exists());
}

#[cfg(all(unix, debug_assertions))]
#[test]
fn a_deletion_stays_in_the_folder_it_checked() {
    // The target is `link/Backups`, `link` leading to `a`; `b` has a
    // folder of the same name, holding a hosted file.
    let folder = Folder::new("race");
    let (a, b) = (folder.0.join("a/Backups"), folder.0.join("b/Backups"));
    let checked = plant_backup(&a, "Every", "0900");
    let config = std::fs::read_to_string(checked.join("backup.config")).unwrap();
    std::fs::write(
        checked.join("backup.config"),
        config.replace("IsPreserved=false", "IsPreserved=true"),
    )
    .unwrap();
    let other = b.join("Every_2026-10-28_0900");
    std::fs::create_dir_all(&other).unwrap();
    let built = TempFile::new("race", &[], true);
    let file = TempFile(other.join("Sample.fmp12"));
    std::fs::rename(&built.0, &file.0).unwrap();
    let link = folder.0.join("link");
    std::os::unix::fs::symlink(folder.0.join("a"), &link).unwrap();
    let clock = ManualClock::new(at(8, 0));
    let (_, handle) = start(&file, &link.join("Backups"), vec![], &clock);
    let id = handle.backups()[0].id.clone();

    // Between the check and the removal, the link moves to `b`.
    let (moved_link, to) = (link.clone(), folder.0.join("b"));
    handle.before_backup_removal(move || {
        let _ = std::fs::remove_file(&moved_link);
        let _ = std::os::unix::fs::symlink(&to, &moved_link);
    });
    handle.delete_backup(&id).unwrap();
    assert!(!checked.exists());
    assert!(file.0.exists() && other.exists());
}

#[cfg(unix)]
#[test]
fn container_data_that_cannot_be_read_fails_the_backup_and_keeps_the_last() {
    use std::os::unix::fs::PermissionsExt;
    let folder = Folder::new("unreadable");
    let file = hosted_file(&folder);
    let containers = folder.0.join("db/RC_Data_FMS/Sample");
    std::fs::create_dir_all(&containers).unwrap();
    std::fs::write(containers.join("a.png"), b"png").unwrap();
    let backups = folder.0.join("Backups");
    let clock = ManualClock::new(at(8, 59));
    let (_, handle) = start(&file, &backups, vec![backup_schedule("Every", 1)], &clock);
    move_to(&handle, &clock, at(9, 0));
    assert!(last_run(&handle, "Every").succeeded());

    let mode = |bits| std::fs::set_permissions(&containers, std::fs::Permissions::from_mode(bits));
    mode(0o000).unwrap();
    // Root reads it anyway: nothing to see then.
    let unreadable = std::fs::read_dir(&containers).is_err();
    move_to(&handle, &clock, at(9, 1));
    mode(0o755).unwrap();
    if unreadable {
        let failed = last_run(&handle, "Every");
        assert!(!failed.succeeded() && failed.error != 0, "{failed:?}");
        assert_eq!(names(&backups), ["Every_2026-10-29_0900"]);
    }

    // A link where the container data should be: not followed, and the
    // backup fails rather than leave it out.
    let elsewhere = folder.0.join("elsewhere");
    std::fs::rename(&containers, &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &containers).unwrap();
    move_to(&handle, &clock, at(9, 2));
    let failed = last_run(&handle, "Every");
    assert!(!failed.succeeded() && failed.error != 0, "{failed:?}");
    assert_eq!(names(&backups), ["Every_2026-10-29_0900"]);
}

#[cfg(debug_assertions)]
#[test]
fn a_backup_whose_folder_cannot_be_flushed_fails_and_keeps_the_older_ones() {
    let folder = Folder::new("flush");
    let file = hosted_file(&folder);
    let backups = folder.0.join("Backups");
    let clock = ManualClock::new(at(8, 59));
    let (_, handle) = start(&file, &backups, vec![backup_schedule("Every", 1)], &clock);
    move_to(&handle, &clock, at(9, 0));
    handle.failing_next_backup_flush();
    move_to(&handle, &clock, at(9, 1));
    let failed = last_run(&handle, "Every");
    assert!(
        !failed.succeeded() && failed.error == 800 && failed.result.is_empty(),
        "{failed:?}"
    );
    // The backup stays, and no older one was deleted.
    assert_eq!(
        names(&backups),
        ["Every_2026-10-29_0900", "Every_2026-10-29_0901"]
    );
    move_to(&handle, &clock, at(9, 2));
    assert!(last_run(&handle, "Every").succeeded());
    assert_eq!(names(&backups), ["Every_2026-10-29_0902"]);
}
