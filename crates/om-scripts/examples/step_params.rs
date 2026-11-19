//! Check of the typed script model against every script stored in local
//! files (script-steps.md 8). For each script catalog it:
//!
//! - decodes every definition with `ScriptCatalog::decode`;
//! - classifies each step's flag bits, step value and data leaves against
//!   the parameter schema (`docs/re/catalogs/script-step-params.csv` plus
//!   the corpus answers of script-steps.md) and lists what stays
//!   unclassified per step id;
//! - re-encodes every definition (`Script::encode`) and compares it with the
//!   stored nodes byte for byte: step list, every step data leaf, metadata;
//! - checks the cached block indexes against `analyze_blocks` and that
//!   recomputing them changes nothing;
//! - re-encodes every calculation and text leaf from its typed value to see
//!   whether FileMaker's chunking is reproduced.
//!
//! `cargo run --release -p om-scripts --example step_params -- <file.fmp12>...`
//! (`@list` reads paths from a file, one per line).
use om_hbam::{HbamFile, Node, OwnedNode};
use om_scripts::{
    analyze_blocks, bytes_node, node_text, step_name, text_node, Calculation, Script,
    ScriptCatalog, ScriptKind, Unclassified,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
struct Coverage {
    steps: usize,
    typed: usize,
    disabled: usize,
    leaves: usize,
    leaves_ok: usize,
    flag_bits: usize,
    flag_bits_ok: usize,
    unclassified: BTreeMap<String, usize>,
}

#[derive(Default)]
struct Totals {
    files: usize,
    catalogs: usize,
    errors: Vec<String>,
    kinds: BTreeMap<String, usize>,
    definitions: usize,
    definitions_exact: usize,
    lists_exact: usize,
    differences: Vec<String>,
    unreferenced: usize,
    empty_keys: usize,
    flow_ok: usize,
    flow_bad: Vec<String>,
    numeric_ok: usize,
    numeric_bad: Vec<String>,
    unbalanced: Vec<String>,
    recompute_noop: usize,
    recompute_changed: Vec<String>,
    calcs: usize,
    calcs_canonical: usize,
    texts: usize,
    texts_canonical: usize,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Leaf paths (segments below the step's subtree) of a node.
fn leaves(node: &OwnedNode) -> Vec<Vec<Vec<u8>>> {
    fn go(node: &OwnedNode, path: &mut Vec<Vec<u8>>, out: &mut Vec<Vec<Vec<u8>>>) {
        if node.value.is_some() {
            out.push(path.clone());
        }
        for child in &node.children {
            path.push(child.segment.clone());
            go(child, path, out);
            path.pop();
        }
    }
    let mut out = Vec::new();
    go(node, &mut Vec::new(), &mut out);
    out
}

/// Whether a leaf path lies in the part of the subtree an unclassified item
/// names.
fn under(path: &[Vec<u8>], item: &Unclassified) -> bool {
    let starts = |prefix: &[Vec<u8>]| path.len() >= prefix.len() && path[..prefix.len()] == *prefix;
    match item {
        Unclassified::Flag(_) | Unclassified::Value(_) => false,
        Unclassified::Slot(n) => path == [n.to_be_bytes().to_vec()],
        Unclassified::SlotChild(n, k) => starts(&[n.to_be_bytes().to_vec(), vec![*k]]),
        Unclassified::CalculationChild(n, seg) => {
            starts(&[n.to_be_bytes().to_vec(), vec![5], seg.clone()])
        }
        Unclassified::Key(k) => starts(&[vec![*k]]),
        Unclassified::Node(p) => starts(p),
    }
}

/// Compare two trees and describe the first difference.
fn first_difference(a: &OwnedNode, b: &OwnedNode, path: &mut Vec<String>) -> Option<String> {
    if a.segment != b.segment || a.value != b.value || a.push != b.push {
        return Some(format!(
            "{} : {}{:?} push {} vs {}{:?} push {}",
            path.join("/"),
            hex(&a.segment),
            a.value.as_deref().map(hex),
            a.push,
            hex(&b.segment),
            b.value.as_deref().map(hex),
            b.push
        ));
    }
    if a.children.len() != b.children.len() {
        let names = |n: &OwnedNode| {
            n.children
                .iter()
                .map(|c| hex(&c.segment))
                .collect::<Vec<_>>()
                .join(",")
        };
        return Some(format!(
            "{} children [{}] vs [{}]",
            path.join("/"),
            names(a),
            names(b)
        ));
    }
    for (x, y) in a.children.iter().zip(&b.children) {
        path.push(hex(&x.segment));
        if let Some(d) = first_difference(x, y, path) {
            return Some(d);
        }
        path.pop();
    }
    None
}

/// Calculation and text leaves re-encoded from their typed values.
fn canonical(node: &OwnedNode, t: &mut Totals) {
    for slot in node.children.iter().filter(|c| c.segment.len() == 2) {
        for child in &slot.children {
            match child.segment.as_slice() {
                [5] => {
                    t.calcs += 1;
                    if Calculation::decode(child).map(|c| c.encode(&[5])).as_ref() == Some(child) {
                        t.calcs_canonical += 1;
                    }
                }
                [1] => {
                    t.texts += 1;
                    if node_text(child).map(|s| text_node(&[1], &s)).as_ref() == Some(child) {
                        t.texts_canonical += 1;
                    }
                }
                _ => {}
            }
        }
    }
}

fn check_script(file: &str, node: &Node<'_>, cov: &mut BTreeMap<u16, Coverage>, t: &mut Totals) {
    let script = match Script::decode(node) {
        Ok(s) => s,
        Err(e) => {
            t.errors.push(format!("{file}: {}", e.0));
            return;
        }
    };
    let label = format!("{file} | {}", script.name);
    *t.kinds.entry(format!("{:?}", script.kind)).or_default() += 1;
    t.definitions += 1;
    let stored = node.to_owned_node();
    let encoded = script.encode();
    match first_difference(&encoded, &stored, &mut Vec::new()) {
        None => t.definitions_exact += 1,
        Some(d) => t.differences.push(format!("{label}: {d}")),
    }
    if script.kind == ScriptKind::Script {
        let list = node.get(4).map(Node::to_owned_node);
        if list.as_ref() == Some(&bytes_node(&[4], &script.encoded_steps())) {
            t.lists_exact += 1;
        }
    }
    t.unreferenced += script.unreferenced_data.len();
    for step in &script.steps {
        let id = step.id();
        let c = cov.entry(id).or_default();
        c.steps += 1;
        if step.data_key.is_empty() {
            t.empty_keys += 1;
        }
        if !step.is_enabled() {
            c.disabled += 1;
        }
        let unclassified = step.unclassified();
        if unclassified.is_empty() {
            c.typed += 1;
        }
        for item in &unclassified {
            *c.unclassified.entry(item.to_string()).or_default() += 1;
        }
        let bits = step.flags.count_ones() as usize;
        c.flag_bits += bits;
        c.flag_bits_ok += bits
            - unclassified
                .iter()
                .filter(|u| matches!(u, Unclassified::Flag(_)))
                .count();
        if let Some(data) = step.data_node() {
            canonical(&data, t);
            for leaf in leaves(&data) {
                c.leaves += 1;
                if !unclassified.iter().any(|u| under(&leaf, u)) {
                    c.leaves_ok += 1;
                }
            }
        }
    }
    match analyze_blocks(&script.steps) {
        Ok(cached) => {
            for (i, (step, cached)) in script.steps.iter().zip(&cached).enumerate() {
                if let Some(v) = cached.value {
                    if v == step.value {
                        t.flow_ok += 1;
                    } else {
                        t.flow_bad.push(format!(
                            "{label} step {} ({}): stored {} expected {v}",
                            i + 1,
                            step.name(),
                            step.value
                        ));
                    }
                }
                if let Some(n) = cached.numeric {
                    match step.number(0) {
                        Some(s) if s == i64::from(n) => t.numeric_ok += 1,
                        // Go to Record without the leaf: nothing cached.
                        None if n == -1 && step.id() != 125 => {}
                        s => t.numeric_bad.push(format!(
                            "{label} step {} ({}): stored {s:?} expected {n}",
                            i + 1,
                            step.name()
                        )),
                    }
                }
            }
            let mut copy = script.clone();
            copy.update_partner_indexes().expect("balanced");
            if copy == script {
                t.recompute_noop += 1;
            } else {
                t.recompute_changed.push(label.clone());
            }
        }
        Err(e) => t.unbalanced.push(format!("{label}: {}", e.0)),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut paths = Vec::new();
    for arg in std::env::args().skip(1) {
        match arg.strip_prefix('@') {
            Some(list) => paths.extend(std::fs::read_to_string(list)?.lines().map(str::to_owned)),
            None => paths.push(arg),
        }
    }
    let mut cov: BTreeMap<u16, Coverage> = BTreeMap::new();
    let mut t = Totals::default();
    for path in paths.iter().filter(|p| !p.trim().is_empty()) {
        let name = std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let file = HbamFile::open(path)?;
        let tree = file.key_tree()?;
        t.files += 1;
        if let Err(e) = ScriptCatalog::decode(&tree) {
            t.errors.push(format!("{name}: catalog: {}", e.0));
        }
        let Some(definitions) = tree.at(&[17, 5]) else {
            continue;
        };
        t.catalogs += 1;
        for node in definitions
            .children
            .iter()
            .filter(|n| n.id().is_some_and(|id| id != 0))
        {
            check_script(&name, node, &mut cov, &mut t);
        }
    }

    println!("files {}, with a script catalog {}", t.files, t.catalogs);
    println!(
        "definitions {} {:?}; decode errors {}",
        t.definitions,
        t.kinds,
        t.errors.len()
    );
    for e in t.errors.iter().take(10) {
        println!("  error {e}");
    }
    println!(
        "re-encode: definitions exact {}/{}, step lists exact {}; unreferenced step data subtrees kept {}; steps with an empty data key {}",
        t.definitions_exact, t.definitions, t.lists_exact, t.unreferenced, t.empty_keys
    );
    for d in t.differences.iter().take(10) {
        println!("  differs {d}");
    }
    println!(
        "block indexes: step values {} match, {} differ; numeric indexes {} match, {} differ; unbalanced scripts {}; recompute no-op {}/{}",
        t.flow_ok,
        t.flow_bad.len(),
        t.numeric_ok,
        t.numeric_bad.len(),
        t.unbalanced.len(),
        t.recompute_noop,
        t.recompute_noop + t.recompute_changed.len()
    );
    for d in t
        .flow_bad
        .iter()
        .chain(&t.numeric_bad)
        .chain(&t.unbalanced)
        .chain(&t.recompute_changed)
        .take(20)
    {
        println!("  {d}");
    }
    println!(
        "typed re-encode: calculations {}/{}, texts {}/{}",
        t.calcs_canonical, t.calcs, t.texts_canonical, t.texts
    );
    println!("id  name                                steps  typed disabled  leaves_ok/leaves  flags_ok/flags");
    let (mut steps, mut typed, mut l, mut lo, mut f, mut fo) = (0, 0, 0, 0, 0, 0);
    let mut unnamed = BTreeSet::new();
    for (id, c) in &cov {
        let name = step_name(*id).unwrap_or("?");
        if name == "?" {
            unnamed.insert(*id);
        }
        println!(
            "{id:3} {name:35} {:6} {:6} {:6} {:8}/{:<8} {:7}/{:<7}",
            c.steps, c.typed, c.disabled, c.leaves_ok, c.leaves, c.flag_bits_ok, c.flag_bits
        );
        for (item, n) in &c.unclassified {
            println!("      unclassified {item} x{n}");
        }
        steps += c.steps;
        typed += c.typed;
        (l, lo, f, fo) = (
            l + c.leaves,
            lo + c.leaves_ok,
            f + c.flag_bits,
            fo + c.flag_bits_ok,
        );
    }
    println!(
        "total: {} step ids ({} unnamed: {unnamed:?}), {steps} steps, fully typed {typed}, partially {}; leaves {lo}/{l}, flag bits {fo}/{f}",
        cov.len(),
        unnamed.len(),
        steps - typed
    );
    Ok(())
}
