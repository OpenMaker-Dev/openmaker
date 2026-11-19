//! End to end: the memory a server script's ExecuteSQL takes (execute-sql.md
//! implementation notes, "Interruption" and "Memory"). A Cartesian product
//! is filtered row by row, so a selective WHERE over a large one holds only
//! what it keeps while its script's time limit runs out; a result larger
//! than the query's memory budget ends the query with error 2 at once,
//! rather than when the time limit is reached.
//!
//! One test per binary: it measures the process's peak memory, which
//! `getrusage` reports on Linux and macOS alike.
#![cfg(unix)]

#[path = "fixtures/tasks.rs"]
mod fixture;
#[path = "fixtures/server.rs"]
mod server;

use std::time::{Duration, Instant};

use fixture::{exit_with, script, set_variable, TempFile, FILE};
use om_server::ServerIdentity;
use om_server::{HostedFile, Identity, ScriptLimits, Server, ServerConfig};

/// The peak resident memory of this process, in bytes: `getrusage`'s
/// `ru_maxrss`, which Linux counts in KiB (`VmHWM`) and macOS in bytes.
fn peak_memory() -> u64 {
    use nix::sys::resource::{getrusage, UsageWho};
    let peak = u64::try_from(getrusage(UsageWho::RUSAGE_SELF).unwrap().max_rss()).unwrap();
    if cfg!(target_os = "macos") {
        peak
    } else {
        peak * 1024
    }
}

/// A server with script limits (Admin has [Full Access]).
fn start(file: &TempFile, limits: ScriptLimits) -> u16 {
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
        .with_script_limits(limits);
    let port = server.local_addr().unwrap().port();
    std::thread::spawn(move || server.serve());
    port
}

#[test]
fn cross_joins_hold_what_they_keep_and_fail_past_their_budget() {
    // 200 tasks: a three-way product of 8 million rows.
    let scripts = [
        script(
            4,
            "Filtered",
            vec![
                set_variable(
                    "$r",
                    "ExecuteSQLe ( \"SELECT COUNT(*) FROM Tasks a, Tasks b, Tasks c WHERE \
                     a.\\\"Count\\\" + b.\\\"Count\\\" + c.\\\"Count\\\" < 0\" ; \"\" ; \"\" )",
                ),
                exit_with("$r"),
            ],
        ),
        script(
            5,
            "Unfiltered",
            vec![
                set_variable(
                    "$r",
                    "ExecuteSQLe ( \"SELECT a.Name FROM Tasks a, Tasks b, Tasks c\" ; \"\" ; \"\" )",
                ),
                exit_with("$r"),
            ],
        ),
    ];
    let mut tree = fixture::tree(&scripts, true);
    fixture::add_tasks(&mut tree, 3..=200);
    let file = TempFile::from_tree("sql-memory", &tree);
    let port = start(
        &file,
        ScriptLimits {
            concurrent: 2,
            steps: u64::MAX,
            duration: Duration::from_secs(4),
        },
    );
    let (mut client, _) = server::logged_in(port, "sql-memory");
    let before = peak_memory();
    // The filtered product runs until the time limit aborts its script;
    // it never held the product (8 million rows of three instances would
    // be over 500 MB).
    let reply = client.perform_script_on_server(4, "", true).unwrap();
    assert_eq!(reply.error, 1, "{reply:?}");
    let grown = peak_memory().saturating_sub(before);
    assert!(grown < 128 << 20, "the filtered join took {grown} bytes");

    // Without a WHERE the rows are the result: past the 256 MiB budget the
    // query ends with error 2, before the time limit.
    let started = Instant::now();
    let reply = client.perform_script_on_server(5, "", true).unwrap();
    assert_eq!(
        (reply.error, reply.result.as_str()),
        (0, "? ERROR:  (2): Memory allocation failure")
    );
    assert!(started.elapsed() < Duration::from_secs(4), "{started:?}");
    client.disconnect().unwrap();
}
