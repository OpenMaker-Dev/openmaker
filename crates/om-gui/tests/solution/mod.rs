//! A synthetic multi-file solution for the startup benchmark
//! (`startup_load.rs`), and what the servers' recordings say the window
//! sent while it opened it.
//!
//! Every file is the Tasks fixture (`om-server/tests/fixtures/tasks.rs`)
//! grown by a [`Shape`]: text fields and records in its Tasks table, and,
//! for the root, layouts. The root's data sources name the related files,
//! one occurrence of each file's Tasks table and a cartesian relationship
//! to it; its first layout shows the related Name of the first
//! [`Shape::shown`] files. Each related file names the next file on its
//! host, so the planner meets files it already opened. Related files on a
//! second host are named by an `fmnet` path to that host's port, as
//! FileMaker Pro stores one (file-metadata.md 1.1). Generic structure
//! only: no customer file, name or data.
//!
//! A test using it declares `mod common; mod fixture; mod hosted; mod
//! two_files;`.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::fixture::{self, NAME, OCCURRENCE, TABLE};
use crate::two_files::{condition_when, control, frame, key, place, reindex, replace};
use om_hbam::slot::uint_segment;
use om_hbam::{serial, OwnedNode};

/// What the synthetic solution holds.
#[derive(Clone, Copy, Debug)]
pub struct Shape {
    /// Related files the root's data sources name.
    pub related: usize,
    /// How many of them, the last ones, a second host has.
    pub on_second_host: usize,
    /// Text fields added to each file's Tasks table.
    pub fields: u64,
    /// Records of each file's Tasks table (at least the fixture's 2).
    pub records: u64,
    /// Layouts of the root besides its first.
    pub layouts: u64,
    /// Fields placed on each of those layouts.
    pub objects: u64,
    /// Related files whose Tasks::Name the root's first layout shows.
    pub shown: usize,
    /// Scripts of the root besides its first, each of 10 Set Variable
    /// steps of 250 characters of text (500 scripts are 2.8 MB).
    pub scripts: u64,
    /// The root relates to each related file by Name `=` Name, its value
    /// index giving the records, rather than by `×` (#1100).
    pub indexed: bool,
    /// [`START`] also has, at x 820, an edit box of Tasks::Name whose
    /// conditional format reads the Name of the related file after the
    /// shown ones, which nothing else on it reads (#1120).
    pub unseen: bool,
    /// [`START`] also has, in view, two edit boxes of Tasks::Name whose
    /// conditional formats read only names of the two related files after
    /// the shown ones: `GetBaseTableName` of the first's Name and
    /// `GetFieldName` of the second's (#1123). The root also has the script
    /// [`NAMES`], which shows the same of the two files after those.
    pub names: bool,
    /// Text fields of the root's Tasks, each auto-entering the Name of one
    /// related file that nothing else reads: [`rule_file`]'s. Only a write's
    /// rules read these files, and planning one opens them (#1139).
    pub write_rules: usize,
}

/// The related file [`Shape::write_rules`]' field `k` (from 0) reads: those
/// after the shown files and the 4 that [`Shape::unseen`] and
/// [`Shape::names`] read.
pub fn rule_file(shape: &Shape, k: usize) -> usize {
    shape.shown + 5 + k
}

/// Where [`Shape::unseen`]'s edit box starts: outside a window narrower
/// than about 820 points.
pub const UNSEEN_LEFT: f64 = 820.0;

pub const ROOT: &str = "Root.fmp12";
/// [`Shape::names`]' script: a dialog with `names:`, then
/// `GetBaseTableName` of the Name of the related file 3 after the shown
/// ones and `GetFieldName` of the one 4 after, separated by `|`.
pub const NAMES: &str = "Names";
/// The root's first layout.
pub const START: &str = "Start";
/// [`Shape::names`]' second script, reading and writing related files
/// nothing else opens. Set Variable [$read] reads the Name of the file 7
/// after the shown ones (#1008). Set Field writes into the Name of the
/// file 5 after them, and Set Field By Name, by a literal name, into the
/// Name of the file 6 after them (#1149). Then a dialog with `write:`, the
/// two Names, `$read` and the last step's error.
pub const WRITE: &str = "Write related";

/// Related file `i` (from 1), without its extension.
pub fn related_name(i: usize) -> String {
    format!("Related {i:02}")
}

/// The root's occurrence of related file `i`'s Tasks table.
pub fn occurrence(i: usize) -> u64 {
    0xd1_0000 + i as u64
}

/// Whether related file `i` is on the second host.
pub fn on_second_host(shape: &Shape, i: usize) -> bool {
    i > shape.related - shape.on_second_host
}

/// The stored reference of a data source (`[32, 5, S, 82]`): a count,
/// then each path's scheme, location and file (file-metadata.md 1.1).
fn reference(paths: &[(&str, &str, &str)]) -> Vec<u8> {
    let mut value = (paths.len() as u32).to_be_bytes().to_vec();
    for (scheme, location, file) in paths {
        for text in [scheme, location, file] {
            value.extend(om_scsu::encode_counted(text));
        }
    }
    value
}

/// Name data source `source` (`name`, `paths`) in `root`, with occurrence
/// `id` (`occurrence_name`) of its Tasks table.
fn data_source(
    root: &mut OwnedNode,
    source: u64,
    name: &str,
    paths: &[(&str, &str, &str)],
    (id, occurrence_name): (u64, &str),
) {
    let source_key = uint_segment(source);
    root.put(
        &[&[32], &[5], &source_key, &[0x10]],
        om_scripts::text_node(&[], name),
    );
    root.put(
        &[&[32], &[5], &source_key, &[0x82]],
        OwnedNode::leaf(Vec::new(), reference(paths)),
    );
    fixture::add_occurrence(root, id, occurrence_name, TABLE);
    root.put(
        &[&[3], &[17], &[5], &uint_segment(id), &[2]],
        OwnedNode::leaf(Vec::new(), [key(source), key(TABLE)].concat()),
    );
}

/// Every file's common part: the fixture with `scripts` and `shape`'s
/// fields and records.
fn grown(shape: &Shape, scripts: &[om_scripts::Script]) -> OwnedNode {
    let mut root = fixture::tree(scripts, true);
    for k in 0..shape.fields {
        fixture::add_field(
            &mut root,
            TABLE,
            0x100 + k,
            &format!("Field {k}"),
            1,
            1,
            0,
            0,
            0,
        );
    }
    if shape.records > 2 {
        fixture::add_tasks(&mut root, 3..=shape.records);
    }
    root
}

/// Related file `i`, naming the next file on its host (none for the last).
pub fn related(shape: &Shape, i: usize) -> OwnedNode {
    let mut root = grown(shape, &[]);
    if shape.indexed {
        fixture::set_field_options(&mut root, TABLE, NAME, 0x8000_0000);
    }
    let next = i + 1;
    let same_host =
        next <= shape.related && on_second_host(shape, next) == on_second_host(shape, i);
    if same_host {
        let name = related_name(next);
        data_source(
            &mut root,
            1,
            &name,
            &[("file", "", &name)],
            (0xd1_0001, &format!("{name} Tasks")),
        );
    }
    root
}

/// The root: its data sources name every related file, its first layout
/// [`START`] shows the first `shape.shown` files' related Name, and
/// `shape.layouts` more layouts show its own fields. `second` is the
/// second host's port.
pub fn root(shape: &Shape, second: Option<u16>) -> OwnedNode {
    // The root's OnFirstWindowOpen script, as a solution's startup script:
    // a window reads the root's scripts as it opens, and runs it. It asks
    // the host for the time (op82), so the trace shows when it ran.
    let script = fixture::script(
        1,
        "Start",
        vec![fixture::set_variable(
            "$$started",
            "Get ( CurrentHostTimestamp )",
        )],
    );
    let text = format!("\"{}\"", "x".repeat(250));
    let scripts: Vec<om_scripts::Script> = std::iter::once(script)
        .chain((0..shape.scripts).map(|n| {
            let steps = (0..10)
                .map(|k| fixture::set_variable(&format!("$step{k}"), &text))
                .collect();
            fixture::script(2 + n, &format!("Script {n:03}"), steps)
        }))
        .chain(names_script(shape))
        .chain(write_script(shape))
        .collect();
    let mut root = grown(shape, &scripts);
    root.put(
        &[&[2], &[0x18]],
        OwnedNode::leaf(Vec::new(), preferences(1)),
    );
    // Every file FileMaker makes has the script catalog's index.
    fixture::index_scripts(&mut root);
    for i in 1..=shape.related {
        let name = related_name(i);
        let location = match second.filter(|_| on_second_host(shape, i)) {
            Some(port) => ("fmnet", format!("\u{E000}127.0.0.1:{port}\u{E000}")),
            None => ("file", String::new()),
        };
        data_source(
            &mut root,
            i as u64,
            &name,
            &[(location.0, &location.1, &name)],
            (occurrence(i), &format!("{name} Tasks")),
        );
        // Every related record, or those of the same Name: the first one
        // is the related Name shown.
        // A write's rules read related records through `=` only (#187):
        // through ×, planning a write is refused, so [`Shape::write_rules`]'
        // files relate by Name, as a solution's lookups do.
        let equal = shape.indexed || (0..shape.write_rules).any(|k| rule_file(shape, k) == i);
        fixture::add_relationship(
            &mut root,
            u8::try_from(i).expect("at most 255 relationships"),
            (OCCURRENCE, NAME),
            if equal { 0 } else { 6 },
            (occurrence(i), NAME),
        );
    }
    add_write_rules(&mut root, shape);
    // Planning a layout reads the layout index.
    fixture::index_layouts(&mut root);
    let start = start_layout(&root, shape);
    root.put(&[&[4], &[5], &start.segment.clone()], start);
    for n in 0..shape.layouts {
        let member = other_layout(&root, n, shape.objects);
        root.put(&[&[4], &[5], &member.segment.clone()], member);
    }
    fixture::index_layouts(&mut root);
    fixture::index_layout_definitions(&mut root);
    // Two themes, as the Contacts starter has, its layouts naming the
    // first.
    fixture::add_themes(&mut root, &[1, 2]);
    root
}

/// [`Shape::write_rules`]' fields: `Rule k`, its value auto-entered as a
/// calculation of `Related NN Tasks::Name`, through the root's `×` or
/// Name = Name relationship to that file, as #1139's tests have it.
fn add_write_rules(root: &mut OwnedNode, shape: &Shape) {
    // The field option word's auto-entered calculated value (schema-path.md
    // 5.5), and the member holding its formula.
    const CALCULATED_VALUE: u32 = 0x8;
    const AUTO_ENTER_FORMULA: u8 = 5;
    for k in 0..shape.write_rules {
        let i = rule_file(shape, k);
        assert!(i <= shape.related, "write rule {k} reads file {i}");
        // After the shape's own text fields (`grown`'s `0x100 + k`), which
        // a rule's id would otherwise replace.
        let id = 0x100 + shape.fields + k as u64;
        let name = format!("Rule {k}");
        fixture::add_field(root, TABLE, id, &name, 1, 1, 0, CALCULATED_VALUE, 0);
        let occurrence_name = format!("{} Tasks", related_name(i));
        let mut names = fixture::names();
        names.add_occurrence(occurrence(i), &occurrence_name, TABLE);
        names.add_field(TABLE, id, &name);
        fixture::add_formula(
            root,
            &names,
            (TABLE, OCCURRENCE),
            id,
            AUTO_ENTER_FORMULA,
            &format!("{occurrence_name}::Name"),
        );
    }
}

/// [`NAMES`], when the shape has [`Shape::names`] and the files.
fn names_script(shape: &Shape) -> Option<om_scripts::Script> {
    let (base, field) = (shape.shown + 3, shape.shown + 4);
    if !shape.names || field > shape.related {
        return None;
    }
    let mut names = fixture::names();
    for i in [base, field] {
        names.add_occurrence(occurrence(i), &format!("{} Tasks", related_name(i)), TABLE);
    }
    let text = format!(
        "\"names:\" & GetBaseTableName ( {} Tasks::Name ) & \"|\" & GetFieldName ( {} Tasks::Name )",
        related_name(base),
        related_name(field)
    );
    let mut ctx = om_formula::Context::new(&names);
    ctx.occurrence = Some(OCCURRENCE);
    let compiled = om_formula::compile(&text, &ctx).unwrap_or_else(|e| panic!("{text}: {e:?}"));
    let mut show = om_scripts::ScriptStep::new(87);
    show.set_text(0, Some("OK"));
    show.set_calculation(
        1,
        Some(&om_scripts::Calculation::new(
            Some(om_formula::formatted::encode(&compiled.tokens)),
            om_formula::polish::encode(&compiled.expr),
        )),
    );
    Some(fixture::script(1000, NAMES, vec![show]))
}

/// [`WRITE`], when the shape has [`Shape::names`] and the files.
fn write_script(shape: &Shape) -> Option<om_scripts::Script> {
    let (target, by_name, read) = (shape.shown + 5, shape.shown + 6, shape.shown + 7);
    if !shape.names || read > shape.related {
        return None;
    }
    let mut names = fixture::names();
    let tasks = format!("{} Tasks", related_name(target));
    let named = format!("{} Tasks", related_name(by_name));
    let readable = format!("{} Tasks", related_name(read));
    names.add_occurrence(occurrence(target), &tasks, TABLE);
    names.add_occurrence(occurrence(by_name), &named, TABLE);
    names.add_occurrence(occurrence(read), &readable, TABLE);
    let calc = |text: &str| {
        let mut ctx = om_formula::Context::new(&names);
        ctx.occurrence = Some(OCCURRENCE);
        let compiled = om_formula::compile(text, &ctx).unwrap_or_else(|e| panic!("{text}: {e:?}"));
        om_scripts::Calculation::new(
            Some(om_formula::formatted::encode(&compiled.tokens)),
            om_formula::polish::encode(&compiled.expr),
        )
    };
    let mut show = om_scripts::ScriptStep::new(87);
    show.set_text(0, Some("OK"));
    show.set_calculation(
        1,
        Some(&calc(&format!(
            "\"write:\" & {tasks}::Name & \":\" & {named}::Name & \":\" & $read & \":\" & Get ( LastError )"
        ))),
    );
    // Set Variable [ $read ; <file> Tasks::Name ]: the step's own read
    // opens the file, and the variable holds its value (#1008).
    let mut read_step = om_scripts::ScriptStep::new(141);
    read_step.set_text(0, Some("$read"));
    read_step.set_calculation(1, Some(&calc(&format!("{readable}::Name"))));
    let write = fixture::set_field_in(occurrence(target), fixture::NAME, "\"written\"");
    // Set Field By Name [ "<file> Tasks::Name" ; "named" ]: the target is a
    // literal, so nothing reads the file before the step writes into it.
    let mut by_name_step = om_scripts::ScriptStep::new(147);
    by_name_step.set_calculation(0, Some(&fixture::calc("\"named\"")));
    by_name_step.set_calculation(1, Some(&fixture::calc(&format!("\"{named}::Name\""))));
    by_name_step.flags |= 0x0800_0000; // kFieldSpecByCalc.
    Some(fixture::script(
        1001,
        WRITE,
        vec![fixture::on(86, true), read_step, write, by_name_step, show],
    ))
}

/// The document preferences `[2, 18]` (file-metadata.md 3.1) of a file
/// whose OnFirstWindowOpen script is `script`: the default options, field
/// view order, OnFirstWindowOpen, no OnLastWindowClose and layout 1 as the
/// unchecked "Switch to layout"; the optional members after it left out.
fn preferences(script: u64) -> Vec<u8> {
    let key = serial::encode(script);
    let mut out = 0x8403u32.to_be_bytes().to_vec();
    out.extend([1, 1]);
    out.push(key.len() as u8 + 1);
    out.push(key.len() as u8);
    out.extend(&key);
    out.push(0);
    out.extend([0x02, 0x01, 0x01]);
    out
}

/// A new layout `name` on Tasks, its id `id`.
fn new_layout(root: &OwnedNode, name: &str, id: u64) -> OwnedNode {
    let catalog = root.at(&[&[4]]).cloned().unwrap();
    let planned = om_data::layout_catalog::plan(
        &catalog,
        &om_data::layout_catalog::NewLayout {
            name: name.into(),
            occurrence: OCCURRENCE,
            theme: vec![1],
            width: 1000.0,
        },
    )
    .unwrap_or_else(|e| panic!("layout {name}: {e}"));
    let mut member = planned.member;
    member.segment = uint_segment(id);
    member
}

/// Layout 1, [`START`]: Tasks::Name, then a related Name per row.
fn start_layout(root: &OwnedNode, shape: &Shape) -> OwnedNode {
    let mut member = new_layout(root, START, fixture::LAYOUT);
    place(&mut member, frame(20.0, 20.0, 40.0, 300.0));
    for i in 1..=shape.shown.min(shape.related) {
        let top = 30.0 + 22.0 * i as f64;
        let at = frame(top, 20.0, top + 20.0, 300.0);
        let object = place(&mut member, at.clone());
        replace(
            &mut member,
            object,
            control(object, &at, (occurrence(i), NAME), 0, None),
            true,
        );
        reindex(&mut member, object, (occurrence(i), NAME));
    }
    let next = shape.shown + 1;
    if shape.names && next < shape.related {
        for (n, (i, function)) in [(next, "GetBaseTableName"), (next + 1, "GetFieldName")]
            .into_iter()
            .enumerate()
        {
            let top = 140.0 + 24.0 * n as f64;
            let object = place(&mut member, frame(top, 400.0, top + 20.0, 560.0));
            let name = format!("{} Tasks", related_name(i));
            let mut names = fixture::names();
            names.add_occurrence(occurrence(i), &name, TABLE);
            condition_when(
                &mut member,
                object,
                &names,
                &format!("{function} ( {name}::Name ) = \"x\""),
            );
        }
    }
    if shape.unseen && next <= shape.related {
        let object = place(
            &mut member,
            frame(52.0, UNSEEN_LEFT, 72.0, UNSEEN_LEFT + 160.0),
        );
        let name = format!("{} Tasks", related_name(next));
        let mut names = fixture::names();
        names.add_occurrence(occurrence(next), &name, TABLE);
        condition_when(
            &mut member,
            object,
            &names,
            &format!("IsEmpty ( {name}::Name )"),
        );
    }
    member
}

/// Another layout of the root (id 3 and on): `objects` edit boxes of
/// Tasks::Name, in columns of 24 inside its 600-point body and 1000-point
/// width (at most 192).
fn other_layout(root: &OwnedNode, n: u64, objects: u64) -> OwnedNode {
    let mut member = new_layout(root, &format!("Layout {n:03}"), 3 + n);
    for object in 0..objects.min(192) {
        let top = 20.0 + 24.0 * (object % 24) as f64;
        let left = 20.0 + 120.0 * (object / 24) as f64;
        place(&mut member, frame(top, left, top + 20.0, left + 110.0));
    }
    member
}

/// Where the hosted copies and recordings go: RAM when the system has it,
/// so a busy disk does not time the server's syncs.
pub fn scratch(name: &str) -> PathBuf {
    let base = Path::new("/dev/shm");
    let base = if base.is_dir() {
        base.to_path_buf()
    } else {
        std::env::temp_dir()
    };
    let dir = base.join(format!(
        "om-{name}-{}-{}",
        std::process::id(),
        crate::two_files::rand_suffix()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A loopback relay to a port, delaying every chunk each way as a link
/// with that one-way latency would ([`relay`]). Dropping it stops it
/// accepting; its connections end with the server's.
pub struct Relay {
    pub port: u16,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        // Wake the accept loop so that it sees the flag.
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

/// A loopback relay to `port` that delays every chunk `delay` each way.
pub fn relay(port: u16, delay: Duration) -> Relay {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let local = listener.local_addr().unwrap().port();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stopped = std::sync::Arc::clone(&stop);
    std::thread::spawn(move || {
        for client in listener.incoming() {
            if stopped.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }
            let Ok(client) = client else { continue };
            let Ok(server) = TcpStream::connect(("127.0.0.1", port)) else {
                continue;
            };
            let _ = client.set_nodelay(true);
            let _ = server.set_nodelay(true);
            let directions = [
                (client.try_clone().unwrap(), server.try_clone().unwrap()),
                (server, client),
            ];
            for (mut from, mut to) in directions {
                let (sent, delayed) = mpsc::channel::<(Instant, Vec<u8>)>();
                std::thread::spawn(move || {
                    let mut buffer = vec![0; 64 * 1024];
                    while let Ok(n @ 1..) = from.read(&mut buffer) {
                        if sent.send((Instant::now(), buffer[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                });
                std::thread::spawn(move || {
                    for (at, bytes) in delayed {
                        let due = at + delay;
                        let now = Instant::now();
                        if due > now {
                            std::thread::sleep(due - now);
                        }
                        if to.write_all(&bytes).is_err() {
                            break;
                        }
                    }
                    let _ = to.shutdown(Shutdown::Write);
                });
            }
        }
    });
    Relay { port: local, stop }
}

/// One request a client sent: when (since the watch started, to the poll
/// that first saw it), its connection and file, its operation (`opNN` for
/// a Perform's transaction, else the GIOP operation), the first two
/// segments of a download's key path, and the bytes each way (the reply's
/// fragments included).
#[derive(Clone, Debug)]
pub struct Sent {
    pub at: Duration,
    pub host: usize,
    pub conn: usize,
    pub file: String,
    pub op: String,
    pub key: String,
    pub sent: usize,
    pub received: usize,
}

impl Sent {
    /// The operation and, for a download, its catalog.
    pub fn what(&self) -> String {
        let op = self
            .op
            .strip_prefix("op")
            .and_then(|n| n.parse::<u8>().ok())
            .and_then(om_protocol::Opcode::name)
            .map_or_else(|| self.op.clone(), |name| format!("{} {name}", self.op));
        if self.key.is_empty() {
            op
        } else {
            format!("{op} [{}]", self.key)
        }
    }
}

/// Watches recording folders (one per host): the size each connection's
/// client stream had at each poll, so each request gets the time of the
/// first poll that saw it whole.
pub struct Watch {
    dirs: Vec<PathBuf>,
    started: Instant,
    /// By host and connection folder: (time, client stream length) at
    /// each change.
    seen: BTreeMap<(usize, PathBuf), Vec<(Duration, u64)>>,
    /// Connections of earlier opens, left out.
    ignored: std::collections::BTreeSet<(usize, PathBuf)>,
    total: (u64, u64),
}

impl Watch {
    pub fn new(dirs: Vec<PathBuf>) -> Self {
        Self {
            dirs,
            started: Instant::now(),
            seen: BTreeMap::new(),
            ignored: std::collections::BTreeSet::new(),
            total: (0, 0),
        }
    }

    /// Leave out the connections seen so far: an earlier open's.
    pub fn ignore_seen(&mut self) {
        let seen = std::mem::take(&mut self.seen);
        self.ignored.extend(seen.into_keys());
    }

    pub fn restart(&mut self) {
        self.started = Instant::now();
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Look at every connection's streams; whether anything grew.
    pub fn poll(&mut self) -> bool {
        let at = self.started.elapsed();
        let mut total = (0, 0);
        for (host, dir) in self.dirs.iter().enumerate() {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let conn = entry.path();
                if self.ignored.contains(&(host, conn.clone())) {
                    continue;
                }
                let c2s = std::fs::metadata(conn.join("c2s.bin")).map_or(0, |m| m.len());
                let s2c = std::fs::metadata(conn.join("s2c.bin")).map_or(0, |m| m.len());
                total.0 += c2s;
                total.1 += s2c;
                let sizes = self.seen.entry((host, conn)).or_default();
                if sizes.last().map_or(c2s > 0, |(_, last)| *last != c2s) {
                    sizes.push((at, c2s));
                }
            }
        }
        let grew = total != self.total;
        self.total = total;
        grew
    }

    /// Every request recorded so far, in the order sent.
    pub fn requests(&self) -> Vec<Sent> {
        let mut all = Vec::new();
        for ((host, conn), sizes) in &self.seen {
            let index = conn
                .file_name()
                .and_then(|n| n.to_str()?.strip_prefix("conn-")?.parse().ok())
                .unwrap_or(usize::MAX);
            let c2s = std::fs::read(conn.join("c2s.bin")).unwrap_or_default();
            let s2c = std::fs::read(conn.join("s2c.bin")).unwrap_or_default();
            all.extend(connection(*host, index, &c2s, &s2c, sizes));
        }
        all.sort_by_key(|sent| (sent.at, sent.host, sent.conn));
        all
    }
}

/// GIOP messages of a stream with the offset each ends at.
fn messages(stream: &[u8]) -> Vec<(&[u8], u64)> {
    let mut out = Vec::new();
    let mut at = 0;
    while let Ok((_, length)) = om_giop::parse_header(&stream[at..]) {
        let Some(message) = stream.get(at..at + length) else {
            break;
        };
        at += length;
        out.push((message, at as u64));
    }
    out
}

/// One connection's requests: each matched with its reply's bytes by
/// request id, a Fragment counted with the reply it continues, and each
/// named by its file as `Captures::transactions` names them (om-provider
/// tests): an Open by the name it sent, any other Perform by its header's
/// file id the Open's reply gave.
fn connection(
    host: usize,
    conn: usize,
    c2s: &[u8],
    s2c: &[u8],
    sizes: &[(Duration, u64)],
) -> Vec<Sent> {
    let mut replies: BTreeMap<u32, usize> = BTreeMap::new();
    let mut opened: BTreeMap<u32, u32> = BTreeMap::new();
    let mut last_reply = None;
    for (message, _) in messages(s2c) {
        let Ok((header, _)) = om_giop::parse_header(message) else {
            continue;
        };
        if header.msg_type == om_giop::MsgType::Fragment {
            if let Some(id) = last_reply {
                *replies.entry(id).or_default() += message.len();
            }
            continue;
        }
        let Ok(reply) = om_giop::Reply::parse(message) else {
            continue;
        };
        last_reply = Some(reply.request_id);
        *replies.entry(reply.request_id).or_default() += message.len();
        if let Ok(open) = om_protocol::parse_perform_reply(message)
            .map_err(|_| ())
            .and_then(|perform| om_protocol::parse_open_reply(&perform.result).map_err(|_| ()))
        {
            opened.insert(reply.request_id, open.file_id);
        }
    }
    let mut names: BTreeMap<u32, String> = BTreeMap::new();
    let mut out = Vec::new();
    for (message, end) in messages(c2s) {
        let Ok(request) = om_giop::Request::parse(message) else {
            continue;
        };
        let at = sizes
            .iter()
            .find(|(_, length)| *length >= end)
            .map_or(Duration::MAX, |(at, _)| *at);
        let mut sent = Sent {
            at,
            host,
            conn,
            file: String::new(),
            op: String::from_utf8_lossy(request.operation)
                .trim_end_matches('\0')
                .to_owned(),
            key: String::new(),
            sent: message.len(),
            received: replies.get(&request.request_id).copied().unwrap_or(0),
        };
        if let Some(perform) = om_protocol::parse_perform_request(request.body) {
            let t = perform.transaction;
            let op = t.first().copied().unwrap_or(0);
            sent.op = format!("op{op}");
            if op == om_protocol::Opcode::OpenFile as u8 {
                if let Ok(open) = om_protocol::parse_open_request(t) {
                    if let Some(id) = opened.get(&request.request_id) {
                        names.insert(*id, open.filename.clone());
                    }
                    sent.file = open.filename;
                }
            } else if let Some(id) = t.get(12..16) {
                let id = u32::from_be_bytes([id[0], id[1], id[2], id[3]]);
                sent.file = names.get(&id).cloned().unwrap_or_default();
            }
            let path = match op {
                90 => om_protocol::parse_download_request(t)
                    .map(|d| d.key_path)
                    .ok(),
                92 => om_protocol::parse_download_list_request(t)
                    .map(|d| d.key_path)
                    .ok(),
                _ => None,
            };
            if let Some(path) = path {
                sent.key = path
                    .iter()
                    .take(2)
                    .map(|s| {
                        serial::decode(s)
                            .or_else(|| om_hbam::slot::be_uint(s))
                            .map_or_else(
                                || s.iter().map(|b| format!("{b:02x}")).collect(),
                                |n| n.to_string(),
                            )
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
            }
        }
        out.push(sent);
    }
    out
}

/// Requests and bytes received, summed by `group`, largest count first.
pub fn sums<K: Ord + Clone>(
    requests: &[Sent],
    group: impl Fn(&Sent) -> K,
) -> Vec<(K, usize, usize, usize)> {
    let mut by: BTreeMap<K, (usize, usize, usize)> = BTreeMap::new();
    for sent in requests {
        let entry = by.entry(group(sent)).or_default();
        entry.0 += 1;
        entry.1 += sent.sent;
        entry.2 += sent.received;
    }
    let mut out: Vec<_> = by
        .into_iter()
        .map(|(k, (n, sent, received))| (k, n, sent, received))
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}
