//! Two files built in memory and hosted by OpenMaker Server on loopback
//! for #804: Orders, whose first layout shows records of Catalog's large
//! table through relationships, a portal and value lists, and helpers that
//! count the requests in the server's recording of the connection. A test
//! using it declares `mod common; mod fixture; mod hosted;` (`fixture` is
//! `om-server/tests/fixtures/tasks.rs`).
#![allow(dead_code)] // Each test binary uses a different subset.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::common;
use crate::fixture::{self, COUNT, LAYOUT, NAME, OCCURRENCE, TABLE};
use om_hbam::slot::uint_segment;
use om_hbam::{serial, OwnedNode};
use om_layout::encode;
use om_layout::Rect;

/// Records of Catalog's Tasks table.
pub const CATALOG_RECORDS: u64 = 20_000;
/// Records of Orders' Tasks table.
pub const ORDERS_RECORDS: u64 = 30;
/// Orders' occurrences of Catalog's Tasks: every record (a cartesian
/// relationship), and the records whose Count equals the order's.
pub const ALL: u64 = 0xd0_0040;
pub const BY_COUNT: u64 = 0xd0_0041;
/// Orders' value lists: Count values shown by Name (second field only),
/// and Name values, both from Catalog through `ALL`.
pub const LABELS: u64 = 1;
pub const NAMES: u64 = 2;
/// Orders' Tasks::Plug, an unstored calculation calling a plug-in function
/// no client has.
pub const PLUG: u64 = 30;
/// Orders' Tasks::Related Sum, an unstored calculation `Sum ( Catalog
/// All::Count )` (all of Catalog's Count values, the same for every order),
/// and Tasks::Related Total, a summary: the Total of Related Sum.
pub const RELATED_SUM: u64 = 31;
pub const RELATED_TOTAL: u64 = 32;
/// Related Total's hide condition: hidden while every order is found.
pub const HIDDEN_TOTAL_WHEN: &str = "Get ( FoundCount ) > 29";

/// What Orders' first layout shows, besides Tasks::Name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Shown {
    /// Catalog All::Name: the first record of a cartesian relationship.
    RelatedField,
    /// A 5-row portal of Catalog By Count, showing Name.
    Portal,
    /// A pop-up menu on Tasks::Count showing `LABELS` (second field only).
    LabelMenu,
    /// A drop-down list on Tasks::Name showing `NAMES`, with an
    /// OnObjectEnter trigger.
    NameList,
    /// A button labelled "Add" (Perform Script).
    AddButton,
    /// Tasks::Plug.
    Plug,
    /// Tasks::Related Total, hidden by `HIDDEN_TOTAL_WHEN`: computing it
    /// reads every record of Catalog.
    HiddenTotal,
    /// Tasks::Related Total, shown: the found set's total of every order's
    /// sum of Catalog's Count values.
    Total,
}

pub fn key(id: u64) -> Vec<u8> {
    let segment = uint_segment(id);
    [vec![segment.len() as u8 + 1, segment.len() as u8], segment].concat()
}

/// A value list definition `[21, 5, V, 02]` (file-metadata.md 6.2): values
/// from field `primary`, also `secondary`, with `options`.
pub fn value_list_definition(
    options: u16,
    primary: (u64, u64),
    secondary: Option<(u64, u64)>,
) -> Vec<u8> {
    let mut out = vec![1];
    out.extend(options.to_be_bytes());
    for spec in [Some(primary), secondary] {
        match spec {
            Some((occurrence, field)) => {
                out.extend(key(occurrence));
                out.extend(key(field));
                out.extend(1u16.to_be_bytes());
            }
            None => out.extend([0, 0, 0, 0]),
        }
    }
    // Start occurrence, external file, external list: none.
    out.extend([0, 0, 0]);
    out.extend(om_scsu::encode_counted(" "));
    out
}

/// A field object as `encode::field_object` writes it, with a control
/// style and value list.
pub fn control(
    key: u64,
    bounds: &Rect,
    field: (u64, u64),
    display: u64,
    list: Option<u64>,
) -> Vec<u8> {
    control_with_flags(key, bounds, field, display, list, encode::ANCHOR_LEFT_TOP)
}

/// [`control`] with the object's `flags` (layout-model.md 5.2.1).
pub fn control_with_flags(
    key: u64,
    bounds: &Rect,
    (occurrence, field): (u64, u64),
    display: u64,
    list: Option<u64>,
    flags: u64,
) -> Vec<u8> {
    let table_key = om_data::layout_placement::serial_value(occurrence).unwrap();
    let field_key = om_data::layout_placement::serial_value(field).unwrap();
    let mut spec = Vec::new();
    encode::uint(1, table_key, &mut spec);
    encode::uint(2, field_key, &mut spec);
    encode::uint(3, 1, &mut spec);
    let mut object = Vec::new();
    // With the icon (`0x400`) a drop-down list shows its arrow.
    let field_flags = encode::EDIT_BOX_FLAGS | if display == 1 { 0x400 } else { 0 };
    encode::uint(1, field_flags, &mut object);
    encode::bytes(2, &spec, &mut object);
    encode::uint(3, 1, &mut object);
    encode::uint(4, 0, &mut object);
    encode::uint(5, display, &mut object);
    if let Some(list) = list {
        encode::uint(
            6,
            om_data::layout_placement::serial_value(list).unwrap(),
            &mut object,
        );
    }
    encode::uint(7, 1, &mut object);
    encode::uint(8, 1, &mut object);
    let style = match display {
        1 => 62,
        2 => 63,
        _ => encode::EDIT_BOX_STYLE,
    };
    let mut out = Vec::new();
    encode::uint(1, u64::from(encode::FIELD), &mut out);
    encode::uint(2, key, &mut out);
    encode::uint(3, flags, &mut out);
    encode::bytes(4, &encode::rect(bounds), &mut out);
    encode::bytes(5, &encode::default_style(style), &mut out);
    out.extend(out_field(&object));
    out
}

pub fn out_field(object: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    encode::bytes(8, object, &mut out);
    out
}

/// A portal of `occurrence` with `rows` rows, holding `child`.
pub fn portal(key: u64, bounds: &Rect, occurrence: u64, rows: f64, child: u64) -> Vec<u8> {
    let mut portal = Vec::new();
    encode::uint(
        2,
        om_data::layout_placement::serial_value(occurrence).unwrap(),
        &mut portal,
    );
    encode::uint(3, 0, &mut portal);
    encode::double(6, rows, &mut portal);
    let mut out = Vec::new();
    encode::uint(1, 9, &mut out);
    encode::uint(2, key, &mut out);
    encode::uint(3, encode::ANCHOR_LEFT_TOP, &mut out);
    encode::bytes(4, &encode::rect(bounds), &mut out);
    encode::bytes(5, &encode::default_style(78), &mut out);
    encode::uint(6, child, &mut out);
    encode::bytes(15, &portal, &mut out);
    out
}

pub fn frame(top: f64, left: f64, bottom: f64, right: f64) -> Rect {
    Rect {
        top,
        left,
        bottom,
        right,
        ..Rect::default()
    }
}

/// Place an edit box of Tasks::Name at `frame` on `member`: the key it got.
pub fn place(member: &mut OwnedNode, frame: Rect) -> u64 {
    let planned = om_data::layout_placement::plan(
        member,
        &om_data::layout_placement::Placement {
            occurrence: OCCURRENCE,
            field: NAME,
            frame,
            label: None,
        },
    )
    .unwrap_or_else(|e| panic!("place: {e}"));
    *member = planned.member;
    planned.field_object
}

/// Replace object `key` of the member's store with `object`; with
/// `top_level` false it is no root (a portal's row field).
pub fn replace(member: &mut OwnedNode, key: u64, object: Vec<u8>, top_level: bool) {
    let store = member
        .child(&[0x07])
        .and_then(|n| n.value.clone())
        .unwrap_or_default();
    let without = encode::without_objects(&store, &[key]).expect("one object and root");
    let roots: Vec<u64> = if top_level { vec![key] } else { Vec::new() };
    let store = encode::append_to_store(&without, &roots, &[object]);
    member.set_child(OwnedNode::leaf(vec![0x07], store));
}

/// Move the field reference index entry of object `key` from Tasks::Name
/// to `occurrence`'s `field`.
pub fn reindex(member: &mut OwnedNode, key: u64, (occurrence, field): (u64, u64)) {
    let object = serial::encode(key);
    member.remove(&[
        &[0x01],
        &uint_segment(OCCURRENCE),
        &uint_segment(NAME),
        &object,
    ]);
    member.put(
        &[
            &[0x01],
            &uint_segment(occurrence),
            &uint_segment(field),
            &object,
        ],
        OwnedNode::leaf(Vec::new(), Vec::new()),
    );
}

/// Take object `key` out of the body part's list of top-level objects.
pub fn unlist(member: &mut OwnedNode, key: u64) {
    let parts = member.child(&[0x03]).cloned().unwrap();
    for part in &parts.children {
        let Some(list) = part.child(&[0x07]).and_then(|n| n.value.clone()) else {
            continue;
        };
        let Some(keys) = part_list(&list) else {
            continue;
        };
        let kept: Vec<u64> = keys.into_iter().filter(|k| *k != key).collect();
        let mut value = (kept.len() as u32).to_be_bytes().to_vec();
        for k in kept {
            let segment = serial::encode(k);
            value.push(segment.len() as u8 + 1);
            value.push(segment.len() as u8);
            value.extend(segment);
        }
        member.put(
            &[&[0x03], &part.segment, &[0x07]],
            OwnedNode::leaf(Vec::new(), value),
        );
    }
}

/// The object keys a part's list `[4, 5, L, 03, P, 07]` holds: a `u32`
/// count, then each key as `[total][length][serial key]`.
pub fn part_list(value: &[u8]) -> Option<Vec<u64>> {
    let mut rest = value.get(4..)?;
    let mut keys = Vec::new();
    while let Some((&total, tail)) = rest.split_first() {
        let entry = tail.get(..usize::from(total))?;
        keys.push(serial::decode(entry.get(1..)?)?);
        rest = &tail[entry.len()..];
    }
    Some(keys)
}

/// Give object `key` the trigger `event` running script `script`.
pub fn trigger(member: &mut OwnedNode, key: u64, event: u16, script: u64) {
    let record = om_layout::triggers::encode_record(&om_layout::ScriptTrigger {
        event,
        script: Some(script),
        file_reference: None,
        external: false,
        modes: 1 | 2,
        option: 0,
        extra: Vec::new(),
        parameter: None,
    });
    member.put(
        &[
            &[0x05],
            &serial::encode(key),
            &[0x30],
            &[5],
            &uint_segment(u64::from(event)),
            &[1],
        ],
        OwnedNode::leaf(Vec::new(), record),
    );
}

/// Give object `key` the hide condition `text`, compiled in Tasks' context
/// with `names`: its formula under attribute key 141 (layout-model.md
/// 5.5.2); its object flags must have `0x4`.
pub fn hide_when(member: &mut OwnedNode, key: u64, names: &om_formula::NameTable, text: &str) {
    let mut ctx = om_formula::Context::new(names);
    ctx.occurrence = Some(OCCURRENCE);
    let compiled = om_formula::compile(text, &ctx).unwrap_or_else(|e| panic!("{text}: {e:?}"));
    for (child, bytes) in [
        (4u8, om_formula::Key::from_id(OCCURRENCE).0),
        (5, om_formula::formatted::encode(&compiled.tokens)),
        (6, om_formula::polish::encode(&compiled.expr)),
    ] {
        member.put(
            &[&[0x05], &serial::encode(key), &[141], &[child]],
            OwnedNode::leaf(Vec::new(), bytes),
        );
    }
}

/// Give object `key` one conditional format, when `text` (compiled in
/// Tasks' context with `names`) holds, with the default format: entry 1 of
/// attribute key 138, its kind (child 1) and its formula (child 5)
/// (layout-model.md 5.5).
pub fn condition_when(member: &mut OwnedNode, key: u64, names: &om_formula::NameTable, text: &str) {
    let mut ctx = om_formula::Context::new(names);
    ctx.occurrence = Some(OCCURRENCE);
    let compiled = om_formula::compile(text, &ctx).unwrap_or_else(|e| panic!("{text}: {e:?}"));
    let object = serial::encode(key);
    member.put(
        &[&[0x05], &object, &[138], &[1], &[1]],
        OwnedNode::leaf(Vec::new(), vec![0]),
    );
    for (child, bytes) in [
        (4u8, om_formula::Key::from_id(OCCURRENCE).0),
        (5, om_formula::formatted::encode(&compiled.tokens)),
        (6, om_formula::polish::encode(&compiled.expr)),
    ] {
        member.put(
            &[&[0x05], &object, &[138], &[1], &[5], &[child]],
            OwnedNode::leaf(Vec::new(), bytes),
        );
    }
}

/// Orders' first layout, showing Tasks::Name and `shown`.
pub fn layout(root: &OwnedNode, shown: &[Shown]) -> OwnedNode {
    let catalog = root.at(&[&[4]]).cloned().unwrap();
    let planned = om_data::layout_catalog::plan(
        &catalog,
        &om_data::layout_catalog::NewLayout {
            name: "Orders".into(),
            occurrence: OCCURRENCE,
            theme: vec![1],
            width: 1000.0,
        },
    )
    .unwrap_or_else(|e| panic!("layout: {e}"));
    let mut member = planned.member;
    member.segment = uint_segment(LAYOUT);
    // List and Table views allowed: `FORM_FLAGS` without bits 4 and 8
    // (layout-model.md 4.1).
    let flags = om_data::layout_catalog::FORM_FLAGS & !(4 | 8);
    member.set_child(OwnedNode::leaf(vec![0xf1], flags.to_be_bytes().to_vec()));
    place(&mut member, frame(20.0, 20.0, 40.0, 300.0));
    for item in shown {
        match item {
            Shown::RelatedField => {
                let key = place(&mut member, frame(60.0, 20.0, 80.0, 300.0));
                replace(
                    &mut member,
                    key,
                    control(key, &frame(60.0, 20.0, 80.0, 300.0), (ALL, NAME), 0, None),
                    true,
                );
                reindex(&mut member, key, (ALL, NAME));
            }
            Shown::Portal => {
                let outer = frame(100.0, 20.0, 230.0, 400.0);
                let row = frame(102.0, 22.0, 124.0, 398.0);
                let holder = place(&mut member, outer.clone());
                let child = place(&mut member, row.clone());
                // A portal's objects are stored relative to the portal.
                let relative = frame(2.0, 2.0, 24.0, 378.0);
                replace(
                    &mut member,
                    child,
                    control(child, &relative, (BY_COUNT, NAME), 0, None),
                    false,
                );
                reindex(&mut member, child, (BY_COUNT, NAME));
                unlist(&mut member, child);
                replace(
                    &mut member,
                    holder,
                    portal(holder, &outer, BY_COUNT, 5.0, child),
                    true,
                );
                member.remove(&[
                    &[0x01],
                    &uint_segment(OCCURRENCE),
                    &uint_segment(NAME),
                    &serial::encode(holder),
                ]);
            }
            Shown::LabelMenu => {
                let at = frame(250.0, 20.0, 270.0, 300.0);
                let key = place(&mut member, at.clone());
                replace(
                    &mut member,
                    key,
                    control(key, &at, (OCCURRENCE, COUNT), 2, Some(LABELS)),
                    true,
                );
                reindex(&mut member, key, (OCCURRENCE, COUNT));
            }
            Shown::NameList => {
                let at = frame(290.0, 20.0, 310.0, 300.0);
                let key = place(&mut member, at.clone());
                replace(
                    &mut member,
                    key,
                    control(key, &at, (OCCURRENCE, NAME), 1, Some(NAMES)),
                    true,
                );
                trigger(&mut member, key, 1, 1);
            }
            Shown::AddButton => {
                let planned = om_data::layout_placement::plan_button(
                    &member,
                    &om_data::layout_placement::ButtonPlacement {
                        frame: frame(330.0, 20.0, 352.0, 120.0),
                        label: "Add".into(),
                        action: om_data::layout_placement::ButtonAction::PerformScript {
                            script: 2,
                            parameter: None,
                        },
                    },
                )
                .unwrap_or_else(|e| panic!("button: {e}"));
                member = planned.member;
            }
            Shown::Plug => {
                let at = frame(370.0, 20.0, 390.0, 300.0);
                let key = place(&mut member, at.clone());
                replace(
                    &mut member,
                    key,
                    control(key, &at, (OCCURRENCE, PLUG), 0, None),
                    true,
                );
                reindex(&mut member, key, (OCCURRENCE, PLUG));
            }
            Shown::Total => {
                let at = frame(490.0, 20.0, 510.0, 300.0);
                let key = place(&mut member, at.clone());
                replace(
                    &mut member,
                    key,
                    control(key, &at, (OCCURRENCE, RELATED_TOTAL), 0, None),
                    true,
                );
                reindex(&mut member, key, (OCCURRENCE, RELATED_TOTAL));
            }
            Shown::HiddenTotal => {
                let at = frame(410.0, 20.0, 430.0, 300.0);
                let key = place(&mut member, at.clone());
                let flags = encode::ANCHOR_LEFT_TOP | 0x4;
                replace(
                    &mut member,
                    key,
                    control_with_flags(key, &at, (OCCURRENCE, RELATED_TOTAL), 0, None, flags),
                    true,
                );
                reindex(&mut member, key, (OCCURRENCE, RELATED_TOTAL));
                hide_when(&mut member, key, &fixture::names(), HIDDEN_TOTAL_WHEN);
            }
        }
    }
    member
}

/// Catalog: Tasks with `CATALOG_RECORDS` records.
pub fn catalog() -> OwnedNode {
    let mut root = fixture::tree(&[], true);
    fixture::add_tasks(&mut root, 3..=CATALOG_RECORDS);
    root
}

/// Orders: Tasks with `ORDERS_RECORDS` records and Catalog's Tasks through
/// `ALL` and `BY_COUNT`, its first layout showing `shown`.
pub fn orders(shown: &[Shown]) -> OwnedNode {
    let scripts = [
        fixture::script(
            1,
            "Entered",
            vec![fixture::set_variable("$$entered", "$$entered + 1")],
        ),
        fixture::script(
            2,
            "Add",
            vec![fixture::set_variable("$$added", "$$added + 1")],
        ),
    ];
    let mut root = fixture::tree(&scripts, true);
    fixture::index_layouts(&mut root);
    fixture::add_tasks(&mut root, 3..=ORDERS_RECORDS);
    // Data source 1, the file "Catalog".
    let mut reference = 1u32.to_be_bytes().to_vec();
    for text in ["file", "", "Catalog"] {
        reference.extend(om_scsu::encode_counted(text));
    }
    root.put(
        &[&[32], &[5], &[1], &[0x82]],
        OwnedNode::leaf(Vec::new(), reference),
    );
    for (occurrence, name) in [(ALL, "Catalog All"), (BY_COUNT, "Catalog By Count")] {
        fixture::add_occurrence(&mut root, occurrence, name, TABLE);
        root.put(
            &[&[3], &[17], &[5], &uint_segment(occurrence), &[2]],
            OwnedNode::leaf(Vec::new(), [key(1), key(TABLE)].concat()),
        );
    }
    fixture::add_relationship(&mut root, 1, (OCCURRENCE, NAME), 6, (ALL, NAME));
    fixture::add_relationship(&mut root, 2, (OCCURRENCE, COUNT), 0, (BY_COUNT, COUNT));
    for (id, name, definition) in [
        (
            LABELS,
            "Labels",
            value_list_definition(0x4, (ALL, COUNT), Some((ALL, NAME))),
        ),
        (NAMES, "Names", value_list_definition(0, (ALL, NAME), None)),
    ] {
        let list = serial::encode(id);
        root.put(
            &[&[0x21], &[5], &list, &[2]],
            OwnedNode::leaf(Vec::new(), definition),
        );
        root.put(
            &[&[0x21], &[5], &list, &[16]],
            om_scripts::text_node(&[], name),
        );
    }
    // Tasks::Plug, `MBS ( "Version" )`: a plug-in function no client has.
    fixture::add_field(&mut root, TABLE, PLUG, "Plug", 2, 1, 0, 0x2_0000, 0);
    let mut names = fixture::names();
    names.add_field(OCCURRENCE, PLUG, "Plug");
    names.add_plugin_function(
        om_formula::PluginFunctionId {
            plugin: *b"MBSP",
            function: 1,
        },
        "MBS",
        1,
        None,
    );
    fixture::add_formula(
        &mut root,
        &names,
        (TABLE, OCCURRENCE),
        PLUG,
        5,
        "MBS ( \"Version\" )",
    );
    if shown.contains(&Shown::HiddenTotal) || shown.contains(&Shown::Total) {
        fixture::add_field(
            &mut root,
            TABLE,
            RELATED_SUM,
            "Related Sum",
            2,
            2,
            0,
            0x2_0000,
            0,
        );
        fixture::add_field(
            &mut root,
            TABLE,
            RELATED_TOTAL,
            "Related Total",
            3,
            2,
            0,
            0,
            0,
        );
        names.add_occurrence(ALL, "Catalog All", TABLE);
        fixture::add_formula(
            &mut root,
            &names,
            (TABLE, OCCURRENCE),
            RELATED_SUM,
            5,
            "Sum ( Catalog All::Count )",
        );
        root.put(
            &[
                &uint_segment(TABLE),
                &[3],
                &[5],
                &uint_segment(RELATED_TOTAL),
                &[9],
            ],
            OwnedNode::leaf(
                Vec::new(),
                fixture::summary_definition(1, 0, RELATED_SUM, None),
            ),
        );
    }
    let member = layout(&root, shown);
    root.put(&[&[4], &[5], &member.segment.clone()], member);
    root
}

/// What a recorded connection's client sent: each request's opcode (the
/// GIOP operation for others), its first two key segments for downloads,
/// and its size.
#[derive(Clone, Debug)]
pub struct Request {
    pub op: String,
    pub key: String,
    pub bytes: usize,
    /// The host's id of the file a Perform names (0 for others).
    pub file: u32,
}

/// The requests in every `conn-*/c2s.bin` under `dir`, and the bytes the
/// server sent back, in order of connection.
pub fn recorded(dir: &Path) -> (Vec<Request>, usize) {
    let mut requests = Vec::new();
    let mut received = 0;
    let mut conns: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|d| d.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    conns.sort();
    for conn in conns {
        received += std::fs::metadata(conn.join("s2c.bin"))
            .map(|m| m.len() as usize)
            .unwrap_or(0);
        let Ok(bytes) = std::fs::read(conn.join("c2s.bin")) else {
            continue;
        };
        let mut reader = om_giop::MessageReader::new(64 << 20);
        let mut cursor = std::io::Cursor::new(bytes);
        while let Ok(message) = reader.read_message(&mut cursor) {
            let Ok(request) = om_giop::Request::parse(&message) else {
                continue;
            };
            let operation = String::from_utf8_lossy(request.operation)
                .trim_end_matches('\0')
                .to_owned();
            let mut entry = Request {
                op: operation.clone(),
                key: String::new(),
                bytes: message.len(),
                file: 0,
            };
            if operation == "Perform" {
                if let Some(perform) = om_protocol::parse_perform_request(request.body) {
                    let t = perform.transaction;
                    let op = t.first().copied().unwrap_or(0);
                    entry.op = format!("op{op}");
                    entry.file = t
                        .get(12..16)
                        .map_or(0, |b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
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
                        entry.key = path
                            .iter()
                            .take(2)
                            .map(|s| s.iter().map(|b| format!("{b:02x}")).collect::<String>())
                            .collect::<Vec<_>>()
                            .join("/");
                    }
                }
            }
            requests.push(entry);
        }
    }
    (requests, received)
}

/// Requests counted by opcode and download key.
pub fn tally(requests: &[Request]) -> BTreeMap<String, usize> {
    let mut by = BTreeMap::new();
    for r in requests {
        let name = if r.key.is_empty() {
            r.op.clone()
        } else {
            format!("{} {}", r.op, r.key)
        };
        *by.entry(name).or_default() += 1;
    }
    by
}

/// Step until the window is settled and nothing new reached the recording
/// for 500 ms.
pub fn quiet(app: &mut common::App, dir: &Path) {
    let start = Instant::now();
    let mut last = (0, 0);
    let mut since = Instant::now();
    loop {
        app.step();
        let (requests, received) = recorded(dir);
        let now = (requests.len(), received);
        if now != last || !app.state().is_settled() {
            last = now;
            since = Instant::now();
        }
        if since.elapsed() > Duration::from_millis(500) {
            app.run_steps(2);
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(300),
            "not quiet after 300 s"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Save the window to `$OM_SHOTS/<name>.png` when that is set.
pub fn shot(app: &mut common::App, name: &str) {
    let Ok(dir) = std::env::var("OM_SHOTS") else {
        return;
    };
    app.run_steps(2);
    if let Ok(image) = app.render() {
        let _ = image.save(format!("{dir}/{name}.png"));
    }
}

pub fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "om-{name}-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn rand_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}
