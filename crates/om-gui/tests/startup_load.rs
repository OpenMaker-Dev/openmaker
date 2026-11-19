//! What opening a hosted multi-file solution costs before its window can be
//! used: the time to the first layout drawn with its records and to a
//! window that is quiet with every value shown, and the requests and bytes
//! on the way, by file, operation and catalog (#772, #804).
//!
//! The solution is synthetic (`solution/mod.rs`): a root file and related
//! files hosted by in-process OpenMaker Servers on loopback, each recording
//! its connections. The window is the real client in the test harness,
//! signing in through the Hosts dialog as a user does. A relay can delay
//! every chunk each way, as a link with that latency would. Cold is the
//! first open in a new window; repeated is the same window opening the file
//! again after File > Close.
//!
//! ```text
//! cargo test --release -p om-gui --test startup_load -- --ignored --nocapture
//! ```
//!
//! `OM_LOAD_RUNS` (default 3) runs each case that many times; the median is
//! reported. `OM_LOAD_CASES` picks cases by name (separated by `;`).
//! `OM_LOAD_TRACE` lists the root file's requests in order, with when.
mod common;
#[path = "../../om-server/tests/fixtures/tasks.rs"]
mod fixture;
mod hosted;
mod solution;
mod two_files;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui_kittest::kittest::Queryable;
use fixture::{ACCOUNT, PASSWORD};
use solution::{Sent, Shape, Watch};

/// A window is usable once it is settled and nothing new reached the
/// servers' recordings for this long.
const QUIET: Duration = Duration::from_millis(500);
/// Every case's limit, from the click on Open.
const LIMIT: Duration = Duration::from_secs(600);

/// A case: the solution's shape and the link's one-way delay.
#[derive(Clone, Copy, Debug)]
struct Case {
    name: &'static str,
    shape: Shape,
    delay: Option<Duration>,
}

/// The shape a large multi-file solution has in miniature: 60 related
/// files, as #804's production-like solution opened 62.
const LARGE: Shape = Shape {
    related: 60,
    on_second_host: 0,
    fields: 60,
    records: 200,
    layouts: 40,
    objects: 4,
    shown: 8,
    scripts: 0,
    indexed: false,
    unseen: false,
    names: false,
    write_rules: 0,
};

fn cases() -> Vec<Case> {
    let ms = |n| Some(Duration::from_millis(n));
    let small = Shape {
        related: 8,
        ..LARGE
    };
    let two_hosts = Shape {
        on_second_host: 20,
        ..LARGE
    };
    // A root whose layout catalog is of the order of #1019's (16 MB): every
    // layout's full definition is downloaded at open.
    let heavy_layouts = Shape {
        layouts: 300,
        objects: 180,
        ..LARGE
    };
    // A root with 500 scripts (2.8 MB), as a solution with hundreds of
    // scripts has (#1052, #1053).
    let heavy_scripts = Shape {
        scripts: 499,
        ..LARGE
    };
    // A first layout showing fields of 8 related files, every file with
    // 1,500 fields: the root's own schema then takes milliseconds to build,
    // as a production-like solution's does, and each shown file's owner
    // plan imports it while the window asks what it may edit.
    let wide = Shape {
        related: 16,
        fields: 1500,
        shown: 8,
        ..LARGE
    };
    // The 8 related Names the first layout shows through Name = Name on a
    // value-indexed Name, rather than ×: the index gives each file's
    // matching record (#1100).
    let indexed = Shape {
        indexed: true,
        ..LARGE
    };
    // The root's Tasks also auto-enters the Name of 4 related files that
    // nothing on the first layout reads: only a write's rules read them,
    // and the window plans its writes as it loads the layout (#1139).
    let write_rules = Shape {
        write_rules: 4,
        ..LARGE
    };
    vec![
        Case {
            name: "8 files, loopback",
            shape: small,
            delay: None,
        },
        Case {
            name: "60 files, loopback",
            shape: LARGE,
            delay: None,
        },
        Case {
            name: "60 files, 1 ms each way",
            shape: LARGE,
            delay: ms(1),
        },
        Case {
            name: "60 files, 10 ms each way",
            shape: LARGE,
            delay: ms(10),
        },
        Case {
            name: "60 files, 20 on a second host, loopback",
            shape: two_hosts,
            delay: None,
        },
        Case {
            name: "60 files, root with 300 layouts of 180 fields, 1 ms each way",
            shape: heavy_layouts,
            delay: ms(1),
        },
        Case {
            name: "60 files, root with 500 scripts, 1 ms each way",
            shape: heavy_scripts,
            delay: ms(1),
        },
        Case {
            name: "16 files, 8 shown, 1,500 fields each, 1 ms each way",
            shape: wide,
            delay: ms(1),
        },
        Case {
            name: "60 files, 8 shown through an indexed Name, 1 ms each way",
            shape: indexed,
            delay: ms(1),
        },
        Case {
            name: "60 files, 8 shown, 4 read only by write rules, 1 ms each way",
            shape: write_rules,
            delay: ms(1),
        },
    ]
}

/// The hosted solution: host 0 has the root and its related files, host 1
/// (when the shape has one) the rest.
struct Hosted {
    hosts: Vec<hosted::Hosted>,
    records: Vec<PathBuf>,
    /// The root's connection, through the relay when there is one.
    connection: om_data::Connection,
    _relay: Option<solution::Relay>,
    _sources: Vec<fixture::TempFile>,
}

fn host(case: &Case) -> Hosted {
    let shape = &case.shape;
    let tag = format!("load-{}", two_files::rand_suffix());
    let file = |name: &str, tree: &om_hbam::OwnedNode| {
        fixture::TempFile::from_tree(&format!("{tag}-{name}"), tree)
    };
    let mut sources = Vec::new();
    let mut hosts = Vec::new();
    let mut records = Vec::new();
    // The second host first: the root names its port.
    let mut second = None;
    if shape.on_second_host > 0 {
        let names: Vec<(fixture::TempFile, String)> = (1..=shape.related)
            .filter(|i| solution::on_second_host(shape, *i))
            .map(|i| {
                let name = solution::related_name(i);
                (
                    file(&name, &solution::related(shape, i)),
                    format!("{name}.fmp12"),
                )
            })
            .collect();
        let record = solution::scratch(&format!("{tag}-b"));
        let files: Vec<(&Path, &str)> = names
            .iter()
            .map(|(source, name)| (source.0.as_path(), name.as_str()))
            .collect();
        let b = hosted::host_files_recorded(
            &format!("{tag}-b"),
            &files,
            (ACCOUNT, PASSWORD),
            |server| server,
            Some(record.clone()),
        );
        second = Some(b.connection.port);
        sources.extend(names.into_iter().map(|(source, _)| source));
        hosts.push(b);
        records.push(record);
    }
    let mut names = vec![(
        file("root", &solution::root(shape, second)),
        solution::ROOT.to_owned(),
    )];
    for i in (1..=shape.related).filter(|i| !solution::on_second_host(shape, *i)) {
        let name = solution::related_name(i);
        names.push((
            file(&name, &solution::related(shape, i)),
            format!("{name}.fmp12"),
        ));
    }
    let record = solution::scratch(&format!("{tag}-a"));
    let files: Vec<(&Path, &str)> = names
        .iter()
        .map(|(source, name)| (source.0.as_path(), name.as_str()))
        .collect();
    let a = hosted::host_files_recorded(
        &format!("{tag}-a"),
        &files,
        (ACCOUNT, PASSWORD),
        |server| server,
        Some(record.clone()),
    );
    sources.extend(names.into_iter().map(|(source, _)| source));
    let mut connection = a.connection.clone();
    let relay = case
        .delay
        .map(|delay| solution::relay(connection.port, delay));
    if let Some(relay) = &relay {
        connection.port = relay.port;
    }
    // Host 0 is the root's.
    hosts.insert(0, a);
    records.insert(0, record);
    Hosted {
        hosts,
        records,
        connection,
        _relay: relay,
        _sources: sources,
    }
}

impl Hosted {
    fn provider(&self) -> om_provider::NativeProvider {
        let others: Vec<&hosted::Hosted> = self.hosts[1..].iter().collect();
        self.hosts[0].provider_trusting(&others)
    }
}

impl Drop for Hosted {
    /// The servers stop, so a later run measures without them.
    fn drop(&mut self) {
        for host in &self.hosts {
            host.shut_down();
        }
        for record in &self.records {
            let _ = std::fs::remove_dir_all(record);
        }
    }
}

/// One open, from the click on Open.
struct Open {
    /// The window has the file: the provider's open ended (Open, Login
    /// and the root's catalogs).
    opened: Duration,
    /// The first layout is drawn with its records.
    painted: Duration,
    /// Settled, every related value the layout shows on screen, and the
    /// last request sent.
    usable: Duration,
    /// Requests sent before the first paint, and in all.
    requests: Vec<Sent>,
    /// The alerts the window showed, dismissed.
    alerts: Vec<String>,
    /// Fields showing the first related record's Name once usable.
    shown: usize,
}

/// Fill in the Hosts dialog for `connection`, replacing what it holds.
fn fill(app: &mut common::App, connection: &om_data::Connection) {
    if app.query_by_label("Use saved license").is_some() {
        common::click(app, "Use saved license");
    }
    hosted::replace(app, "Host", &connection.host);
    hosted::replace(app, "Port", &connection.port.to_string());
    hosted::replace(app, "File", &connection.file);
    hosted::replace(app, "Account", &connection.account);
    hosted::replace(app, "Password", &connection.password);
}

/// The fields showing the first related record's Name: the root's own
/// Name shows the same text, so one less.
fn shown(app: &common::App) -> usize {
    app.query_all_by_value("Write tests")
        .count()
        .saturating_sub(1)
}

/// Open `connection` in `app` and step it until usable: settled, the
/// `expected` related Names shown, and quiet.
fn open(app: &mut common::App, hosted: &Hosted, expected: usize) -> Open {
    fill(app, &hosted.connection);
    let mut watch = Watch::new(hosted.records.clone());
    // Connections of earlier opens are not this one's.
    watch.poll();
    watch.ignore_seen();
    app.get_by_label("Open").click();
    watch.restart();
    let (mut opened, mut painted, mut complete) = (None, None, None);
    let mut alerts = Vec::new();
    let mut last = Duration::ZERO;
    loop {
        app.step();
        let now = watch.elapsed();
        if watch.poll() {
            last = now;
        }
        let state = app.state();
        if opened.is_none() && state.is_open() {
            opened = Some(now);
        }
        if let Some(alert) = state.error().map(str::to_owned) {
            // The alert's button is there from the next frame.
            if app.query_by_label("OK").is_some() {
                alerts.push(alert);
                // A user's click, which closes it once (#981).
                common::click(app, "OK");
                assert!(alerts.len() < 20, "the alert keeps coming back: {alerts:?}");
            }
            assert!(now < LIMIT, "an alert with no OK: {alerts:?}");
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        let settled = app.state().is_settled();
        if settled && painted.is_none() {
            painted = Some(now);
        }
        if !settled {
            last = now;
        }
        // The last reply is recorded before a delayed link delivers it, so
        // the values on screen, not the recordings, say when it arrived.
        if painted.is_some() && complete.is_none() && shown(app) >= expected {
            complete = Some(now);
        }
        if complete.is_some() && now - last > QUIET {
            break;
        }
        assert!(
            now < LIMIT,
            "not usable after {LIMIT:?}: {} of {expected} related values shown, {alerts:?}",
            shown(app)
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    app.run_steps(2);
    Open {
        opened: opened.unwrap_or(Duration::MAX),
        painted: painted.unwrap(),
        usable: last.max(complete.unwrap()),
        requests: watch.requests(),
        alerts,
        shown: shown(app),
    }
}

/// File > Close, back to the Hosts dialog.
fn close(app: &mut common::App) {
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::W);
    common::wait_until(app, "the Hosts dialog", |app| {
        !app.state().is_open() && app.query_by_label("Hosts").is_some()
    });
}

fn ms(duration: Duration) -> String {
    format!("{:.0}", duration.as_secs_f64() * 1000.0)
}

fn kb(bytes: usize) -> String {
    format!("{:.1}", bytes as f64 / 1024.0)
}

/// Whether `sent` is the root file's.
fn root(sent: &Sent) -> bool {
    sent.file.is_empty() || sent.file.eq_ignore_ascii_case(solution::ROOT)
}

/// The report of one open: times, requests by phase and the largest
/// groups by file kind, operation and catalog.
fn report(case: &Case, kind: &str, open: &Open) -> String {
    let before: Vec<&Sent> = open
        .requests
        .iter()
        .filter(|s| s.at <= open.painted)
        .collect();
    let related: Vec<&Sent> = open.requests.iter().filter(|s| !root(s)).collect();
    let files: std::collections::BTreeSet<&str> = related.iter().map(|s| s.file.as_str()).collect();
    let (hydration_start, hydration_end) = (
        related.first().map_or(Duration::ZERO, |s| s.at),
        related.last().map_or(Duration::ZERO, |s| s.at),
    );
    let first_layout = open
        .requests
        .iter()
        .find(|s| root(s) && s.key.starts_with("4"))
        .map_or(Duration::ZERO, |s| s.at);
    let total = |list: &[&Sent]| list.iter().map(|s| s.received).sum::<usize>();
    let mut out = format!(
        "### {} ({kind})\n\nOpened {} ms, first paint {} ms, usable {} ms. \
         Requests before first paint {} ({} KB received), in all {} ({} KB). \
         Related files {} opened by {} requests, from {} ms to {} ms; the root's first \
         layout download at {} ms. Related Names shown when usable: {} of {}.\n",
        case.name,
        ms(open.opened),
        ms(open.painted),
        ms(open.usable),
        before.len(),
        kb(total(&before)),
        open.requests.len(),
        kb(open.requests.iter().map(|s| s.received).sum()),
        files.len(),
        related.len(),
        ms(hydration_start),
        ms(hydration_end),
        ms(first_layout),
        open.shown,
        case.shape
            .shown
            .min(case.shape.related - case.shape.on_second_host),
    );
    if !open.alerts.is_empty() {
        out += &format!("\nAlerts: {}\n", open.alerts.join(" | ").replace('\n', " "));
    }
    out += "\n| File | Operation | Requests | Sent KB | Received KB |\n|---|---|---:|---:|---:|\n";
    let groups = solution::sums(&open.requests, |s| {
        (if root(s) { "root" } else { "related" }, s.what())
    });
    // The 16 largest groups by requests, and every one of the root's: its
    // catalogs are what a startup change moves.
    let groups = groups
        .into_iter()
        .enumerate()
        .filter(|(i, ((file, _), ..))| *i < 16 || *file == "root")
        .map(|(_, group)| group);
    for ((file, what), n, sent, received) in groups {
        out += &format!(
            "| {file} | {what} | {n} | {} | {} |\n",
            kb(sent),
            kb(received)
        );
    }
    // `OM_LOAD_TRACE`: the root's requests in the order sent, with when.
    if std::env::var_os("OM_LOAD_TRACE").is_some() {
        out += "\n| ms | Root request | Received KB |\n|---:|---|---:|\n";
        for sent in open.requests.iter().filter(|s| root(s)) {
            out += &format!(
                "| {} | {} | {} |\n",
                ms(sent.at),
                sent.what(),
                kb(sent.received)
            );
        }
    }
    out
}

/// The median open of `runs` by usable time.
fn median(mut runs: Vec<Open>) -> Open {
    runs.sort_by_key(|run| run.usable);
    let middle = runs.len() / 2;
    runs.swap_remove(middle)
}

#[test]
#[ignore = "a measurement: prints startup times and requests for synthetic multi-file solutions"]
fn startup_times_and_requests() {
    let runs: usize = std::env::var("OM_LOAD_RUNS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(3);
    let wanted: Option<Vec<String>> = std::env::var("OM_LOAD_CASES")
        .ok()
        .map(|names| names.split(';').map(|s| s.trim().to_owned()).collect());
    let mut summary = vec![
        "| Case | Open | Opened ms | First paint ms | Usable ms | Requests to paint | Requests | Received KB |".to_owned(),
        "|---|---|---:|---:|---:|---:|---:|---:|".to_owned(),
    ];
    let mut details = Vec::new();
    for case in cases() {
        if wanted
            .as_ref()
            .is_some_and(|w| !w.iter().any(|n| n == case.name))
        {
            continue;
        }
        for (mode, demand) in modes() {
            let (mut cold, mut repeated) = (Vec::new(), Vec::new());
            for _ in 0..runs {
                let hosted = host(&case);
                let provider = hosted.provider().with_related_on_demand(demand);
                let mut app = common::harness(Box::new(provider));
                let started = Instant::now();
                let expected = case
                    .shape
                    .shown
                    .min(case.shape.related - case.shape.on_second_host);
                cold.push(open(&mut app, &hosted, expected));
                close(&mut app);
                repeated.push(open(&mut app, &hosted, expected));
                close(&mut app);
                eprintln!(
                    "{} ({mode}): run in {:.1} s",
                    case.name,
                    started.elapsed().as_secs_f64()
                );
            }
            for (kind, runs) in [("cold", cold), ("repeated", repeated)] {
                let kind = format!("{kind}, {mode}");
                let kind = kind.as_str();
                let open = median(runs);
                let before = open
                    .requests
                    .iter()
                    .filter(|s| s.at <= open.painted)
                    .count();
                let row = format!(
                    "| {} | {kind} | {} | {} | {} | {before} | {} | {} |",
                    case.name,
                    ms(open.opened),
                    ms(open.painted),
                    ms(open.usable),
                    open.requests.len(),
                    kb(open.requests.iter().map(|s| s.received).sum()),
                );
                // Each case as it ends too: a later case's failure keeps it.
                let detail = report(&case, kind, &open);
                println!("{row}\n\n{detail}");
                summary.push(row);
                details.push(detail);
            }
        }
    }
    println!("{}\n\n{}", summary.join("\n"), details.join("\n"));
}

/// How related files open, from `OM_LOAD_DEMAND`: `eager` (every one when
/// the schema is first built), `demand` (when something first reads them,
/// #983) or `both`, the default.
fn modes() -> Vec<(&'static str, bool)> {
    match std::env::var("OM_LOAD_DEMAND").as_deref() {
        Ok("eager") => vec![("eager", false)],
        Ok("demand") => vec![("on demand", true)],
        _ => vec![("eager", false), ("on demand", true)],
    }
}

/// On demand (#983), the window opens the related files its first layout
/// reads and no other: the root's 6 related files, of which it shows the
/// Name of 2, give 2 Opens besides the root's, both Names shown once the
/// window is usable, and no alert.
#[test]
fn a_window_opens_only_the_related_files_its_layout_reads() {
    let case = Case {
        name: "6 files, 2 shown",
        shape: Shape {
            related: 6,
            on_second_host: 0,
            fields: 2,
            records: 3,
            layouts: 0,
            objects: 0,
            shown: 2,
            scripts: 0,
            indexed: false,
            unseen: false,
            names: false,
            write_rules: 0,
        },
        delay: None,
    };
    let hosted = host(&case);
    let provider = hosted.provider().with_related_on_demand(true);
    let mut app = common::harness(Box::new(provider));
    let open = open(&mut app, &hosted, 2);
    assert!(open.alerts.is_empty(), "{:?}", open.alerts);
    assert_eq!(open.shown, 2, "both related Names shown");
    let mut opened: Vec<&str> = open
        .requests
        .iter()
        .filter(|s| s.op == "op78")
        .map(|s| s.file.as_str())
        .collect();
    opened.sort_unstable();
    // The other 4 were never opened.
    assert_eq!(
        opened,
        ["Related 01.fmp12", "Related 02.fmp12", solution::ROOT]
    );
    // Of the root's layout catalog, the index and the first layout's
    // member, not the whole catalog (#1019).
    let catalog: Vec<&str> = open
        .requests
        .iter()
        .filter(|s| root(s) && (s.key == "4" || s.key.starts_with("4, ")))
        .map(|s| s.key.as_str())
        .collect();
    assert_eq!(catalog, ["4, 1", "4, 5"], "{catalog:?}");
    // Of its script catalog, the index (the calculation schema's script
    // names) and the member of the OnFirstWindowOpen script, which ran:
    // no other script's steps, and not the whole catalog (#1052, #1053).
    let scripts: Vec<&str> = open
        .requests
        .iter()
        .filter(|s| root(s) && (s.key == "17" || s.key.starts_with("17, ")))
        .map(|s| s.key.as_str())
        .collect();
    assert_eq!(scripts, ["17, 1", "17, 5"], "{scripts:?}");
    // Of its theme catalog, the index and the member of the theme the
    // first layout names, not the other theme (#1129).
    let themes: Vec<&str> = open
        .requests
        .iter()
        .filter(|s| root(s) && (s.key == "6" || s.key.starts_with("6, ")))
        .map(|s| s.key.as_str())
        .collect();
    assert_eq!(themes, ["6, 1", "6, 5"], "{themes:?}");
}

/// A formula that only changes how an object looks opens no file while the
/// object is outside the window (#1120). The first layout's edit box at x
/// 820, outside a 760-point-wide window, has a conditional format reading
/// Related 03's Name, which nothing else on it reads: Related 03 does not
/// open with the window, and opens once a wider window shows the box.
#[test]
fn a_conditional_format_outside_the_window_opens_its_file_once_shown() {
    let case = Case {
        name: "6 files, 2 shown, 1 read outside the window",
        shape: Shape {
            related: 6,
            on_second_host: 0,
            fields: 2,
            records: 3,
            layouts: 0,
            objects: 0,
            shown: 2,
            scripts: 0,
            indexed: false,
            unseen: true,
            names: false,
            write_rules: 0,
        },
        delay: None,
    };
    let hosted = host(&case);
    let provider = hosted.provider().with_related_on_demand(true);
    let mut app = common::harness_sized(Box::new(provider), egui::vec2(760.0, 760.0));
    let open = open(&mut app, &hosted, 2);
    assert!(open.alerts.is_empty(), "{:?}", open.alerts);
    let opened = |requests: &[Sent]| {
        let mut files: Vec<String> = requests
            .iter()
            .filter(|s| s.op == "op78")
            .map(|s| s.file.clone())
            .collect();
        files.sort_unstable();
        files
    };
    assert_eq!(
        opened(&open.requests),
        ["Related 01.fmp12", "Related 02.fmp12", solution::ROOT]
    );
    // Wide enough for the whole 1,000-point layout: the box is drawn. The
    // open may go over a connection of the window's first open.
    let mut watch = Watch::new(hosted.records.clone());
    watch.poll();
    let before = opened(&watch.requests());
    app.set_size(egui::vec2(1060.0, 760.0));
    loop {
        app.step();
        watch.poll();
        let mut now = opened(&watch.requests());
        for file in &before {
            let at = now.iter().position(|f| f == file).unwrap();
            now.remove(at);
        }
        if now == ["Related 03.fmp12"] && app.state().is_settled() {
            break;
        }
        assert!(
            watch.elapsed() < Duration::from_secs(60),
            "Related 03 not opened once shown: {now:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A formula that reads only names of a related file's table or field asks
/// for that file like one that reads its data (#1123): two boxes in view
/// whose conditional formats read `GetBaseTableName` of Related 03's Name
/// and `GetFieldName` of Related 04's open both files with the window. A
/// script reading the same of Related 05 and 06 opens them and answers
/// their names, not error 102.
#[test]
fn a_conditional_format_reading_only_names_opens_their_files() {
    use egui_kittest::kittest::By;
    let case = Case {
        name: "6 files, 2 shown, 2 read by name only",
        shape: Shape {
            related: 6,
            on_second_host: 0,
            fields: 2,
            records: 3,
            layouts: 0,
            objects: 0,
            shown: 2,
            scripts: 0,
            indexed: false,
            unseen: false,
            names: true,
            write_rules: 0,
        },
        delay: None,
    };
    let hosted = host(&case);
    let provider = hosted.provider().with_related_on_demand(true);
    let mut app = common::harness(Box::new(provider));
    let open = open(&mut app, &hosted, 2);
    assert!(open.alerts.is_empty(), "{:?}", open.alerts);
    let mut opened: Vec<&str> = open
        .requests
        .iter()
        .filter(|s| s.op == "op78")
        .map(|s| s.file.as_str())
        .collect();
    opened.sort_unstable();
    assert_eq!(
        opened,
        [
            "Related 01.fmp12",
            "Related 02.fmp12",
            "Related 03.fmp12",
            "Related 04.fmp12",
            solution::ROOT
        ]
    );
    app.query_all_by_role_and_label(egui::Role::Button, "Scripts")
        .next()
        .unwrap()
        .click();
    app.run_steps(2);
    common::click(&mut app, solution::NAMES);
    common::wait_until(&mut app, "the names script's dialog", |app| {
        app.query(By::new().label_contains("names:")).is_some() || app.state().error().is_some()
    });
    let expected = "names:Tasks|Related 06 Tasks::Name";
    let labels: Vec<_> = app
        .query_all(By::new().role(egui::Role::Label))
        .filter_map(|n| n.value())
        .collect();
    assert!(
        app.query(By::new().label(expected)).is_some(),
        "expected {expected:?}, error {:?}, labels {labels:?}",
        app.state().error()
    );
    common::click(&mut app, "OK");
}

/// A related file only a write's rules read does not open with the window
/// (#1139): root's Rule 0 is auto-entered by Related 07's Name
/// ([`solution::rule_file`]), which the first layout does not read. The
/// window opens without it, the window's record capabilities offering New
/// Record all the same; the first saved new record opens it and is stored.
#[test]
fn a_file_only_a_writes_rules_read_opens_with_the_first_write() {
    use egui_kittest::kittest::By;
    let case = Case {
        name: "7 files, 2 shown, 1 read by a write's rule",
        shape: Shape {
            related: 7,
            on_second_host: 0,
            fields: 2,
            records: 3,
            layouts: 0,
            objects: 0,
            shown: 2,
            scripts: 0,
            indexed: false,
            unseen: false,
            names: false,
            write_rules: 1,
        },
        delay: None,
    };
    let hosted = host(&case);
    let provider = hosted.provider().with_related_on_demand(true);
    let mut app = common::harness(Box::new(provider));
    let open = open(&mut app, &hosted, 2);
    assert!(open.alerts.is_empty(), "{:?}", open.alerts);
    let opened = |requests: &[Sent]| {
        let mut files: Vec<String> = requests
            .iter()
            .filter(|s| s.op == "op78")
            .map(|s| s.file.clone())
            .collect();
        files.sort_unstable();
        files
    };
    assert_eq!(
        opened(&open.requests),
        ["Related 01.fmp12", "Related 02.fmp12", solution::ROOT],
        "the first layout waits for no file only a write reads"
    );
    let mut watch = Watch::new(hosted.records.clone());
    watch.poll();
    let before = opened(&watch.requests());
    common::click(&mut app, "New Record");
    common::wait_until(&mut app, "the held new record", |a| {
        a.state().is_settled() && common::record_number(a) == "4"
    });
    app.query_all(By::new().role(egui::Role::TextInput).label("Name"))
        .next()
        .unwrap()
        .click();
    app.run_steps(2);
    app.query_all(By::new().role(egui::Role::TextInput).label("Name"))
        .next()
        .unwrap()
        .type_text("Write tests");
    app.run_steps(2);
    common::save_record(&mut app);
    loop {
        app.step();
        watch.poll();
        let mut now = opened(&watch.requests());
        for file in &before {
            let at = now.iter().position(|f| f == file).unwrap();
            now.remove(at);
        }
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        if now
            == [format!(
                "{}.fmp12",
                solution::related_name(solution::rule_file(&case.shape, 0))
            )]
            && app.state().is_settled()
            && app.state().record_edit_state().is_none()
        {
            break;
        }
        assert!(
            watch.elapsed() < Duration::from_secs(60),
            "the first write did not open Related 07: {now:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A script's read of a related file nothing opened yet, and its Set Field
/// and Set Field By Name into such files, open them and read or write
/// there. A Set Variable reading Related 09 holds its stored Name, not `?`
/// (#1008). Related 07 takes "written", and Related 08, named by a literal,
/// "named", rather than failing with 102 (#1149).
#[test]
fn a_script_reading_or_writing_unopened_related_files_opens_them() {
    use egui_kittest::kittest::By;
    let case = Case {
        name: "9 files, 2 shown, 3 read or written by a script only",
        shape: Shape {
            related: 9,
            on_second_host: 0,
            fields: 2,
            records: 3,
            layouts: 0,
            objects: 0,
            shown: 2,
            scripts: 0,
            indexed: false,
            unseen: false,
            names: true,
            write_rules: 0,
        },
        delay: None,
    };
    let hosted = host(&case);
    let provider = hosted.provider().with_related_on_demand(true);
    let mut app = common::harness(Box::new(provider));
    let open = open(&mut app, &hosted, 2);
    assert!(open.alerts.is_empty(), "{:?}", open.alerts);
    for file in ["Related 07.fmp12", "Related 08.fmp12", "Related 09.fmp12"] {
        assert!(
            !open
                .requests
                .iter()
                .any(|s| s.op == "op78" && s.file == file),
            "{file} is not opened with the window"
        );
    }
    app.query_all_by_role_and_label(egui::Role::Button, "Scripts")
        .next()
        .unwrap()
        .click();
    app.run_steps(2);
    common::click(&mut app, solution::WRITE);
    common::wait_until(&mut app, "the write script's dialog", |app| {
        app.query(By::new().label_contains("write:")).is_some() || app.state().error().is_some()
    });
    let expected = "write:written:named:Write tests:0";
    let labels: Vec<_> = app
        .query_all(By::new().role(egui::Role::Label))
        .filter_map(|n| n.value())
        .collect();
    assert!(
        app.query(By::new().label(expected)).is_some(),
        "expected {expected:?}, error {:?}, labels {labels:?}",
        app.state().error()
    );
    common::click(&mut app, "OK");
}
