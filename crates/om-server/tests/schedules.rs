//! Script schedules (script-runtime.md "Implementation notes (schedules)")
//! on an `om-server` running in this process, hosting a file built for the
//! test (`fixtures/tasks.rs`), with a clock the test moves: runs on time,
//! skipped and missed starts, the session a run has (FileMaker Server's
//! answers, script oracle s36), runs that cannot start, time limits, the
//! last run kept per schedule, running one now, enabling, and shutdown.

#[path = "fixtures/tasks.rs"]
mod fixture;

use std::sync::Arc;
use std::time::{Duration, Instant};

use fixture::{TempFile, ACCOUNT, COUNT, FILE, NAME, TABLE};
use jiff::civil::{date, Time};
use jiff::tz::TimeZone;
use jiff::Zoned;
use om_hbam::HbamFile;
use om_server::{
    AbortReason, Days, HostedFile, Identity, ManualClock, Repeat, Schedule, ScheduleError,
    ScheduleStatus, ScheduleTask, ScriptLimits, ScriptTask, Server, ServerConfig, ServerHandle,
    ServerIdentity, Times,
};

const WAIT: Duration = Duration::from_secs(20);

/// 2026-10-29 at `h:m` UTC.
fn at(h: i8, m: i8) -> Zoned {
    date(2026, 10, 29)
        .at(h, m, 0, 0)
        .to_zoned(TimeZone::UTC)
        .unwrap()
}

/// A server hosting `file` with `schedules` on `clock`, serving; the file's
/// accounts are the fixture's (Admin, Clerk).
fn start(
    file: &TempFile,
    schedules: Vec<Schedule>,
    clock: &Arc<ManualClock>,
    limits: ScriptLimits,
) -> ServerHandle {
    let config = ServerConfig {
        identity: ServerIdentity {
            name: "openmaker-test".into(),
            version_string: "OpenMaker Server test".into(),
            server_id: "0123456789abcdef0123456789abcdef".into(),
            web_service_settings: String::new(),
        },
        files: vec![HostedFile {
            name: FILE.into(),
            path: file.0.clone(),
        }],
        insecure_files: false,
        record_dir: None,
    };
    let identity = Identity::self_signed(vec!["localhost".into()]).unwrap();
    let server = Server::bind("127.0.0.1:0", config, identity)
        .unwrap()
        .with_script_limits(limits)
        .with_clock(clock.clone())
        .with_schedules(schedules)
        .unwrap();
    let handle = server.handle().unwrap();
    std::thread::spawn(move || server.serve());
    assert!(
        handle.wait_for_schedules(WAIT),
        "the scheduler did not start"
    );
    handle
}

/// A schedule of `script` as Admin, every 15 minutes from 9:00 to 10:00.
fn schedule(name: &str, script: &str) -> Schedule {
    Schedule {
        name: name.into(),
        task: ScheduleTask::Script(ScriptTask {
            file: FILE.into(),
            script: script.into(),
            account: ACCOUNT.into(),
            parameter: String::new(),
            time_limit: None,
            end_on_time_limit: false,
        }),
        repeat: Repeat::Days {
            days: Days::Daily,
            times: Times::Every {
                interval: Duration::from_secs(15 * 60),
                from: Time::new(9, 0, 0, 0).unwrap(),
                until: Time::new(10, 0, 0, 0).unwrap(),
            },
            start: None,
            end: None,
        },
        enabled: true,
    }
}

/// The script task of `schedule`.
fn task(schedule: &mut Schedule) -> &mut ScriptTask {
    match &mut schedule.task {
        ScheduleTask::Script(task) => task,
        ScheduleTask::Backup(_) => unreachable!("a script schedule"),
    }
}

fn status(handle: &ServerHandle, name: &str) -> ScheduleStatus {
    handle
        .schedules()
        .into_iter()
        .find(|s| s.schedule.name.eq_ignore_ascii_case(name))
        .unwrap()
}

/// Move `clock` and let the scheduler start and finish what is due.
fn move_to(handle: &ServerHandle, clock: &ManualClock, now: Zoned) {
    clock.set(now);
    assert!(handle.wait_for_schedules(WAIT), "runs did not end");
}

/// Wait (in real time; the run's script pauses) until `ready` holds.
fn wait_until(handle: &ServerHandle, name: &str, ready: impl Fn(&ScheduleStatus) -> bool) {
    let deadline = Instant::now() + WAIT;
    while !ready(&status(handle, name)) {
        assert!(Instant::now() < deadline, "{:?}", status(handle, name));
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Run schedule `name` now and wait for its run to end.
fn run_now(handle: &ServerHandle, name: &str) -> om_server::LastRun {
    let before = status(handle, name).runs;
    handle.run_schedule(name).unwrap();
    assert!(handle.wait_for_schedules(WAIT), "the run did not end");
    let after = status(handle, name);
    assert_eq!(after.runs, before + 1);
    after.last_run.unwrap()
}

/// A stored cell of a record in the file on disk.
fn stored(file: &TempFile, record: u64, field: u64) -> Option<String> {
    HbamFile::open(&file.0)
        .unwrap()
        .cell(TABLE, record, field)
        .unwrap()
        .and_then(|cell| cell.text().and_then(Result::ok))
}

fn record_count(file: &TempFile) -> usize {
    HbamFile::open(&file.0)
        .unwrap()
        .records(TABLE)
        .unwrap()
        .len()
}

#[test]
fn runs_on_time_and_does_not_make_up_missed_starts() {
    let file = TempFile::new("sched-clock", &[fixture::server_work()], true);
    let clock = ManualClock::new(at(8, 50));
    let mut every = schedule("Every quarter hour", "Server Work");
    task(&mut every).parameter = "the clock".into();
    let handle = start(&file, vec![every], &clock, ScriptLimits::DEFAULT);
    let first = status(&handle, "Every quarter hour");
    assert_eq!(first.next_run, Some(at(9, 0)));
    assert_eq!((first.runs, first.last_run), (0, None));

    // Not yet.
    move_to(&handle, &clock, at(8, 59));
    assert_eq!(status(&handle, "Every quarter hour").runs, 0);
    // 9:00: the script runs as Admin with the parameter.
    move_to(&handle, &clock, at(9, 0));
    let s = status(&handle, "Every quarter hour");
    assert_eq!(s.runs, 1);
    let last = s.last_run.unwrap();
    assert!(last.succeeded() && !last.manual, "{last:?}");
    assert_eq!((last.started.clone(), last.error), (at(9, 0), 0));
    let parts: Vec<&str> = last.result.split('|').collect();
    assert_eq!(parts[..4], ["3", "3", "From the clock", "Admin"]);
    assert!(parts[4].starts_with("Server "), "{last:?}");
    assert_eq!(s.next_run, Some(at(9, 15)));
    assert_eq!(stored(&file, 1, COUNT).as_deref(), Some("13"));
    assert_eq!(stored(&file, 3, NAME).as_deref(), Some("From the clock"));

    // 9:14: nothing; 9:50: one run, not three (the missed 9:15 to 9:45
    // are not made up), and the next is 10:00.
    move_to(&handle, &clock, at(9, 14));
    assert_eq!(status(&handle, "Every quarter hour").runs, 1);
    move_to(&handle, &clock, at(9, 50));
    let s = status(&handle, "Every quarter hour");
    assert_eq!((s.runs, s.skipped), (2, 0));
    assert_eq!(s.next_run, Some(at(10, 0)));
    // 10:00 is the day's last; then 9:00 tomorrow.
    move_to(&handle, &clock, at(10, 0));
    let s = status(&handle, "Every quarter hour");
    assert_eq!(s.runs, 3);
    assert_eq!(
        s.next_run.unwrap().datetime(),
        date(2026, 10, 30).at(9, 0, 0, 0)
    );
    assert_eq!(stored(&file, 1, COUNT).as_deref(), Some("33"));
    assert_eq!(record_count(&file), 5);
    handle.shutdown(WAIT);
}

#[test]
fn a_run_has_filemaker_servers_session() {
    let context = fixture::script(
        10,
        "Context",
        vec![fixture::exit_with(
            "Get ( ScriptName ) & \"|\" & Get ( ScriptParameter ) & \"|\" & \
             Length ( Get ( ScriptParameter ) ) & \"|\" & Get ( AccountName ) & \"|\" & \
             Get ( UserName ) & \"|\" & Get ( WindowName ) & \"|\" & Get ( ScriptResult ) & \
             \"|\" & Get ( LastError ) & \"|\" & Get ( LayoutName ) & \"|\" & \
             Get ( ErrorCaptureState ) & \"|\" & Get ( AllowAbortState ) & \"|\" & \
             Get ( WindowMode )",
        )],
    );
    let globals = fixture::script(
        11,
        "Globals",
        vec![
            fixture::set_variable("$$g", "$$g + 1"),
            fixture::exit_with("\"g=\" & $$g"),
        ],
    );
    let file = TempFile::new("sched-session", &[context, globals], true);
    let clock = ManualClock::new(at(8, 0));
    // The script named in another case, the account as the schedule
    // writes it, a parameter that is a calculation's text.
    let mut s = schedule("Context", "CONTEXT");
    task(&mut s).account = "admin".into();
    task(&mut s).parameter = "5 + 5".into();
    let mut empty = schedule("No parameter", "context");
    empty.enabled = false;
    let handle = start(
        &file,
        vec![s, empty, schedule("Globals", "Globals")],
        &clock,
        ScriptLimits::DEFAULT,
    );
    let last = run_now(&handle, "Context");
    assert!(last.succeeded() && last.manual && last.ran, "{last:?}");
    assert_eq!(
        last.result,
        "Context|5 + 5|5|admin|admin|Tasks||0|Tasks|0|1|0"
    );
    // A disabled schedule runs when asked to; no parameter is empty text.
    let last = run_now(&handle, "No parameter");
    assert_eq!(last.result, "Context||0|Admin|Admin|Tasks||0|Tasks|0|1|0");
    // `$$` variables do not outlive a run.
    assert_eq!(run_now(&handle, "Globals").result, "g=1");
    assert_eq!(run_now(&handle, "globals").result, "g=1");
    handle.shutdown(WAIT);
}

#[test]
fn runs_that_cannot_start_keep_their_error() {
    // Clerk without a stored password, which no client could sign in with.
    let mut root = fixture::tree(&[fixture::server_work()], true);
    root.remove(&[&[23], &[1], &[5], &[3], &[6]]).unwrap();
    let file = TempFile::from_tree("sched-refused", &root);
    let clock = ManualClock::new(at(8, 0));
    let with = |name: &str, change: &dyn Fn(&mut Schedule)| {
        let mut s = schedule(name, "Server Work");
        change(&mut s);
        s
    };
    let schedules = vec![
        with("Closed file", &|s| task(s).file = "Missing.fmp12".into()),
        // Not an account of the file.
        with("Unknown account", &|s| task(s).account = "Nobody".into()),
        with("No password", &|s| task(s).account = fixture::CLERK.into()),
        with("No script", &|s| task(s).script = "Server Work ".into()),
    ];
    let handle = start(&file, schedules, &clock, ScriptLimits::DEFAULT);
    for (name, ran, error) in [
        ("Closed file", false, 802),
        ("Unknown account", false, 212),
        ("No password", false, 212),
        ("No script", true, 104),
    ] {
        let last = run_now(&handle, name);
        assert_eq!((last.ran, last.error), (ran, error), "{name}: {last:?}");
        assert!(!last.succeeded() && last.result.is_empty());
    }
    // Nothing ran.
    assert_eq!(stored(&file, 1, COUNT).as_deref(), Some("3"));
    assert_eq!(record_count(&file), 2);
    assert_eq!(
        handle.run_schedule("Nothing"),
        Err(ScheduleError::NotFound("Nothing".into()))
    );

    // At the script limit nothing runs either: 812.
    let file = TempFile::new("sched-limit", &[fixture::server_work()], true);
    let limits = ScriptLimits {
        concurrent: 0,
        ..ScriptLimits::DEFAULT
    };
    let handle = start(&file, vec![schedule("Work", "Server Work")], &clock, limits);
    let last = run_now(&handle, "Work");
    assert_eq!((last.ran, last.error), (false, 812));
    assert_eq!(record_count(&file), 2);
}

#[test]
fn a_start_while_the_last_run_executes_is_skipped() {
    // The run pauses until the administrator aborts it.
    let file = TempFile::new("sched-skip", &[fixture::slow(600.0)], true);
    let clock = ManualClock::new(at(8, 59));
    let handle = start(
        &file,
        vec![schedule("Slow", "Slow")],
        &clock,
        ScriptLimits::DEFAULT,
    );
    clock.set(at(9, 0));
    wait_until(&handle, "Slow", |s| s.running_since.is_some());
    assert_eq!(
        handle.run_schedule("Slow"),
        Err(ScheduleError::Running("Slow".into()))
    );
    // 9:15 and 9:30 come while the 9:00 run still pauses.
    move_to_while_running(&handle, &clock, at(9, 15), 1);
    move_to_while_running(&handle, &clock, at(9, 30), 2);
    let script = handle.scripts();
    assert_eq!(script.len(), 1);
    assert_eq!(script[0].script, "Slow");
    assert!(handle.abort_script(script[0].id));
    assert!(handle.wait_for_schedules(WAIT));
    let s = status(&handle, "Slow");
    assert_eq!((s.runs, s.skipped), (1, 2));
    assert_eq!(s.next_run, Some(at(9, 45)));
    let last = s.last_run.unwrap();
    assert_eq!(
        (last.started, last.error, last.aborted),
        (at(9, 0), 1, Some(AbortReason::Admin))
    );
    // The next start runs.
    clock.set(at(9, 45));
    wait_until(&handle, "Slow", |s| s.running_since.is_some());
    assert_eq!(status(&handle, "Slow").skipped, 2);
    handle.shutdown(WAIT);
}

/// Move `clock` while a run executes, until the scheduler skipped `skipped`
/// starts.
fn move_to_while_running(handle: &ServerHandle, clock: &ManualClock, now: Zoned, skipped: u64) {
    clock.set(now);
    wait_until(handle, "Slow", |s| s.skipped == skipped);
    assert!(status(handle, "Slow").running_since.is_some());
}

#[test]
fn time_limits_end_the_run_or_only_warn() {
    let file = TempFile::new("sched-limits", &[fixture::slow(30.0)], true);
    let quick = TempFile::new("sched-limits-quick", &[fixture::slow(0.6)], true);
    let clock = ManualClock::new(at(8, 0));
    let mut ending = schedule("Ending", "Slow");
    task(&mut ending).time_limit = Some(Duration::from_millis(200));
    task(&mut ending).end_on_time_limit = true;
    let handle = start(&file, vec![ending], &clock, ScriptLimits::DEFAULT);
    let started = Instant::now();
    let last = run_now(&handle, "Ending");
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(
        (last.ran, last.error, last.aborted),
        (true, 1, Some(AbortReason::TimeLimit))
    );
    assert!(last.result.is_empty() && !last.succeeded());
    assert!(last.exceeded_time_limit, "{last:?}");
    handle.shutdown(WAIT);

    // An earlier server deadline is not the schedule's own time limit.
    let mut ending = schedule("Server limit", "Slow");
    task(&mut ending).time_limit = Some(Duration::from_secs(10));
    task(&mut ending).end_on_time_limit = true;
    let limits = ScriptLimits {
        duration: Duration::from_millis(100),
        ..ScriptLimits::DEFAULT
    };
    let handle = start(&file, vec![ending], &clock, limits);
    let last = run_now(&handle, "Server limit");
    assert_eq!(last.aborted, Some(AbortReason::TimeLimit));
    assert!(!last.exceeded_time_limit, "{last:?}");
    handle.shutdown(WAIT);

    // Without ending, the run goes on to its end past the limit.
    let mut warning = schedule("Warning", "Slow");
    task(&mut warning).time_limit = Some(Duration::from_millis(100));
    let handle = start(&quick, vec![warning], &clock, ScriptLimits::DEFAULT);
    let last = run_now(&handle, "Warning");
    assert!(last.succeeded() && last.exceeded_time_limit, "{last:?}");
    assert_eq!(last.result, "slow done");
    handle.shutdown(WAIT);
}

#[test]
fn shutdown_aborts_runs_and_starts_no_more() {
    let file = TempFile::new("sched-shutdown", &[fixture::slow(60.0)], true);
    let clock = ManualClock::new(at(8, 59));
    let handle = start(
        &file,
        vec![schedule("Slow", "Slow")],
        &clock,
        ScriptLimits::DEFAULT,
    );
    clock.set(at(9, 0));
    wait_until(&handle, "Slow", |s| s.running_since.is_some());
    let started = Instant::now();
    assert!(handle.shutdown(WAIT));
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(handle.wait_for_schedules(WAIT));
    let s = status(&handle, "Slow");
    let last = s.last_run.unwrap();
    assert_eq!((last.error, last.aborted), (1, Some(AbortReason::Shutdown)));
    assert_eq!(s.next_run, None);
    // No run starts during or after shutdown.
    clock.set(at(9, 30));
    assert!(handle.wait_for_schedules(WAIT));
    assert_eq!(status(&handle, "Slow").runs, 1);
    assert_eq!(
        handle.run_schedule("Slow"),
        Err(ScheduleError::ShuttingDown)
    );
}

#[test]
fn closing_the_file_aborts_its_run() {
    let file = TempFile::new("sched-close", &[fixture::slow(600.0)], true);
    let clock = ManualClock::new(at(8, 0));
    let handle = start(
        &file,
        vec![schedule("Slow", "Slow")],
        &clock,
        ScriptLimits::DEFAULT,
    );
    handle.run_schedule("Slow").unwrap();
    wait_until(&handle, "Slow", |s| s.running_since.is_some());
    assert_eq!(handle.abort_file_scripts(&FILE.to_lowercase()), 1);
    assert!(handle.wait_for_schedules(WAIT));
    let last = status(&handle, "Slow").last_run.unwrap();
    assert_eq!(
        (last.ran, last.error, last.aborted, last.manual),
        (true, 1, Some(AbortReason::FileClosed), true)
    );
    handle.shutdown(WAIT);
}

#[test]
fn disabled_schedules_wait_and_can_be_enabled() {
    let file = TempFile::new("sched-enable", &[fixture::server_work()], true);
    let clock = ManualClock::new(at(8, 0));
    let mut off = schedule("Off", "Server Work");
    off.enabled = false;
    let mut once = schedule("Once", "Server Work");
    once.repeat = Repeat::Once(date(2026, 10, 29).at(12, 0, 0, 0));
    let handle = start(&file, vec![off, once], &clock, ScriptLimits::DEFAULT);
    assert_eq!(status(&handle, "Off").next_run, None);
    move_to(&handle, &clock, at(9, 0));
    assert_eq!(status(&handle, "Off").runs, 0);
    handle.enable_schedule("off", true).unwrap();
    assert_eq!(status(&handle, "Off").next_run, Some(at(9, 15)));
    move_to(&handle, &clock, at(9, 15));
    assert_eq!(status(&handle, "Off").runs, 1);
    handle.enable_schedule("Off", false).unwrap();
    assert_eq!(status(&handle, "Off").next_run, None);
    // Once: at its time, then never again.
    move_to(&handle, &clock, at(12, 0));
    let s = status(&handle, "Once");
    assert_eq!((s.runs, s.next_run), (1, None));
    move_to(&handle, &clock, at(23, 0));
    assert_eq!(status(&handle, "Once").runs, 1);
    assert_eq!(status(&handle, "Off").runs, 1);
    handle.shutdown(WAIT);
}

#[test]
fn schedules_are_checked() {
    let file = TempFile::new("sched-checks", &[], true);
    let config = |schedules: Vec<Schedule>| {
        let config = ServerConfig {
            identity: ServerIdentity {
                name: "openmaker-test".into(),
                version_string: "OpenMaker Server test".into(),
                server_id: "0123456789abcdef0123456789abcdef".into(),
                web_service_settings: String::new(),
            },
            files: vec![HostedFile {
                name: FILE.into(),
                path: file.0.clone(),
            }],
            insecure_files: false,
            record_dir: None,
        };
        let identity = Identity::self_signed(vec!["localhost".into()]).unwrap();
        Server::bind("127.0.0.1:0", config, identity)
            .unwrap()
            .with_schedules(schedules)
            .err()
    };
    assert_eq!(
        config(vec![schedule("A", "x"), schedule("a", "y")]),
        Some(ScheduleError::Duplicate("a".into()))
    );
    let mut every = schedule("Every", "x");
    every.repeat = Repeat::Days {
        days: Days::Every(2),
        times: Times::At(Time::new(1, 0, 0, 0).unwrap()),
        start: None,
        end: None,
    };
    assert!(matches!(
        config(vec![every]),
        Some(ScheduleError::Invalid { .. })
    ));
    assert_eq!(config(vec![schedule("Fine", "x")]), None);
}

/// Runs are recorded in the event log as FileMaker Server records them
/// (`docs/re/server-logs.md` 3): running, the script started, completed
/// and when it runs next; a run the administrator starts, and one whose
/// account cannot log in, too.
#[test]
fn runs_are_recorded_in_the_event_log() {
    let file = TempFile::new("sched-log", &[fixture::server_work()], true);
    let folder = file.0.with_extension("logs");
    let clock = ManualClock::new(at(8, 50));
    let mut ghost = schedule("Ghost", "Server Work");
    task(&mut ghost).account = "Ghost".into();
    ghost.enabled = false;
    let config = ServerConfig {
        identity: ServerIdentity {
            name: "openmaker-test".into(),
            version_string: "OpenMaker Server test".into(),
            server_id: "0123456789abcdef0123456789abcdef".into(),
            web_service_settings: String::new(),
        },
        files: vec![HostedFile {
            name: FILE.into(),
            path: file.0.clone(),
        }],
        insecure_files: false,
        record_dir: None,
    };
    let identity = Identity::self_signed(vec!["localhost".into()]).unwrap();
    let server = Server::bind("127.0.0.1:0", config, identity)
        .unwrap()
        .with_event_log(&folder)
        .unwrap()
        .with_clock(clock.clone())
        .with_schedules(vec![schedule("Every quarter hour", "Server Work"), ghost])
        .unwrap();
    let handle = server.handle().unwrap();
    std::thread::spawn(move || server.serve());
    assert!(handle.wait_for_schedules(WAIT));

    move_to(&handle, &clock, at(9, 0));
    assert_eq!(run_now(&handle, "ghost").error, 212);
    handle.enable_schedule("GHOST", false).unwrap();
    let every = "Schedule \"Every quarter hour\"";
    let expected = [
        format!("Information 148 {every} running."),
        format!("Information 689 {every} has started FileMaker script \"Server Work\"."),
        format!("Information 150 {every} completed."),
        format!("Information 126 {every} scheduled for 10/29/2026 09:15 AM."),
        "Information 0 Schedule \"Ghost\" run now by the administrator.".to_owned(),
        "Information 148 Schedule \"Ghost\" running.".to_owned(),
        "Error 732 Schedule \"Ghost\" aborted; invalid account name or password.".to_owned(),
        "Information 0 Schedule \"Ghost\" disabled by the administrator.".to_owned(),
    ];
    let lines = || -> Vec<String> {
        let page = handle.read_log("Event.log", None, 100).unwrap();
        page.entries
            .iter()
            .rev()
            .filter(|entry| entry.message.starts_with("Schedule"))
            .map(|entry| format!("{} {} {}", entry.level, entry.number, entry.message))
            .collect()
    };
    let deadline = Instant::now() + WAIT;
    while lines().len() < expected.len() {
        assert!(Instant::now() < deadline, "{:?}", lines());
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(lines(), expected);
    handle.shutdown(WAIT);
    std::fs::remove_dir_all(&folder).unwrap();
}
