//! Read-only dump of every stored calculation in local `.fmp12` files, with
//! the names a printer needs. Input for `tools/calc_polish.py`
//! (docs/re/calc-encoding.md section 11).
//!
//! A stored calculation is a key node whose children are `4` (context table
//! occurrence, `KAlias`), `5` (formatted stream, `KFormula`) and `6`
//! (compiled "Polish" stream, `KPolish`); custom functions omit `4` and a
//! file whose formula text was stripped omits `5`. Every row is
//! tab-separated:
//!
//! ```text
//! calc  <file>  <owner>  <path>  <context-hex>  <formatted-hex>  <polish-hex>
//! to    <file>  <occurrence id>  <base table id>  <name>
//! field <file>  <base table id>  <field id>  <name>
//! cf    <file>  <function id>  <name>  <parameters>
//! ```
//!
//! Paths use decimal for segments of 1 to 4 bytes and `x<hex>` otherwise; a
//! missing child is `-`. Names are printed with tabs and newlines escaped.
//!
//! `cargo run -p om-hbam --example dump_calcs -- <file.fmp12>... > calcs.tsv`
use om_hbam::{HbamFile, KeySegment, Node};
use std::io::Write;

fn hex(bytes: Option<&[u8]>) -> String {
    match bytes {
        Some(b) => b.iter().map(|x| format!("{x:02x}")).collect(),
        None => "-".into(),
    }
}

fn clean(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn path_text(path: &[KeySegment<'_>]) -> String {
    path.iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>()
        .join("/")
}

/// The owner of a calculation from its path (docs/re/calc-encoding.md 8).
/// `None` for nodes that only look like calculations: record data, catalog
/// indexes and the dependency index `3/17/1/14`.
fn owner(path: &[KeySegment<'_>]) -> Option<String> {
    let ids: Vec<Option<u64>> = path.iter().map(|s| s.as_uint()).collect();
    let id = |i: usize| ids.get(i).copied().flatten();
    let root = id(0)?;
    let tail = |from: usize| {
        ids[from..]
            .iter()
            .map(|i| i.map_or("?".into(), |n| n.to_string()))
            .collect::<Vec<_>>()
            .join("/")
    };
    Some(match root {
        r if r >= 0x8000 => {
            // Field definitions [T, 3, 5, F, key]; everything else under a
            // table key is record or index data.
            if (id(1), id(2)) != (Some(3), Some(5)) || ids.len() != 5 {
                return None;
            }
            match id(4)? {
                5 => "field-calc",
                25 => "field-validation",
                32 => "field-validation-message",
                39 => "field-storage",
                _ => "field-other",
            }
            .into()
        }
        // Layout script trigger parameter: [4, 5, L, 48, 5, trigger, 49].
        4 if (id(1), id(3), id(4), id(6)) == (Some(5), Some(48), Some(5), Some(49)) => {
            format!("layout-trigger/{}", tail(5))
        }
        // Privilege set record access: [23, 2, 5, P, 5, T, privilege].
        23 if (id(1), id(2), id(4)) == (Some(2), Some(5), Some(5)) => {
            format!("record-access/{}", tail(6))
        }
        // Catalog indexes [cat, 1, ...] (schema-path.md 5.1), including the
        // calculation dependency index 3/17/1/14, hold key strings only.
        _ if id(1) == Some(1) => return None,
        3 if (id(1), id(2), id(3)) == (Some(17), Some(1), Some(14)) => return None,
        // Object attributes: [4, 5, L, 5, O, ...] or [4, 5, L, 13, 5, O, ...].
        4 if id(1) == Some(5) && id(3) == Some(5) && ids.len() >= 6 => {
            format!("layout-object/{}", tail(5))
        }
        4 if id(1) == Some(5) && (id(3), id(4)) == (Some(13), Some(5)) && ids.len() >= 7 => {
            format!("layout-object/{}", tail(6))
        }
        // [16, 5, F, 35]: custom function formula (KFunction).
        16 if ids.len() == 4 && id(3) == Some(35) => "custom-function".into(),
        // [17, 5, S, 5, step, slot, ..., 5]: script step parameter calculation.
        17 if ids.len() >= 7
            && (id(1), id(3), ids[ids.len() - 1]) == (Some(5), Some(5), Some(5)) =>
        {
            format!("script-step/{}", tail(4))
        }
        // [65, 5, M, 37]: custom menu "install when"; deeper: menu items.
        65 if ids.len() >= 4 && id(1) == Some(5) => format!("custom-menu/{}", tail(3)),
        _ => format!("other/{}", tail(0)),
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = std::io::stdout();
    let mut out = out.lock();
    for arg in std::env::args_os().skip(1) {
        let file = HbamFile::open(&arg)?;
        let tree = file.key_tree()?;
        let catalog = om_hbam::Catalog::from_tree(&tree)?;
        let name = std::path::Path::new(&arg)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        for to in &catalog.table_occurrences {
            let base = to.base_table().map_or("-".into(), |t| t.to_string());
            writeln!(out, "to\t{name}\t{}\t{base}\t{}", to.id, clean(&to.name))?;
        }
        for table in &catalog.tables {
            for field in &table.fields {
                writeln!(
                    out,
                    "field\t{name}\t{}\t{}\t{}",
                    table.id,
                    field.id,
                    clean(&field.name)
                )?;
            }
        }
        if let Some(functions) = tree.at(&[16, 5]) {
            for f in &functions.children {
                let text = |key| {
                    f.get(key)
                        .and_then(|n| n.text())
                        .and_then(Result::ok)
                        .unwrap_or_default()
                };
                writeln!(
                    out,
                    "cf\t{name}\t{}\t{}\t{}",
                    f.id().unwrap_or_default(),
                    clean(&text(16)),
                    clean(&text(33))
                )?;
            }
        }
        let mut rows = Vec::new();
        tree.root.walk(&mut |path, node: &Node<'_>| {
            let child = |id| node.get(id).and_then(|n| n.assembled());
            let only_calc = !node.children.is_empty()
                && node.value.is_none()
                && node.children.iter().all(|c| matches!(c.id(), Some(4..=6)));
            if !only_calc {
                return;
            }
            let (context, formatted, polish) = (child(4), child(5), child(6));
            if polish.is_none() {
                return;
            }
            if let Some(owner) = owner(path) {
                // Layout object attribute lists can hold items keyed 4..6
                // that are not calculations; there a stripped formula (no
                // `5`) cannot be told apart, so `5` is required.
                let strict = owner.starts_with("layout-object") || owner.starts_with("other");
                if formatted.is_none() && strict {
                    return;
                }
                rows.push(format!(
                    "calc\t{name}\t{owner}\t{}\t{}\t{}\t{}",
                    path_text(path),
                    hex(context.as_deref()),
                    hex(formatted.as_deref()),
                    hex(polish.as_deref()),
                ));
            }
        });
        for row in rows {
            writeln!(out, "{row}")?;
        }
    }
    Ok(())
}
