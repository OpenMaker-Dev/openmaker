//! Read-only dump of a local file's value and word indexes, record by
//! record: for each indexed field of a table, every record's keys in its
//! value index (`[table, 01, field, 00]`) and word index (`.. 01`), beside
//! the record's value of one field. For probes whose records each hold one
//! value in a field and indexed copies of it in others (query-program.md
//! 7.2; `tools/fms-script-harness/wordbreaks`).
//!
//! Output, tab-separated: field name, its index language, record id, the
//! value field's text with `\r \n \t \\` escaped and invisible code points
//! as `\u{hex}`, the value-index keys and the word-index keys (each hex,
//! sorted, space-separated).
//!
//! `cargo run -p om-hbam --example index_entries -- <file.fmp12> <table> <value field>`
use std::collections::{BTreeMap, BTreeSet};

use om_hbam::slot::uint_segment;
use om_hbam::{HbamFile, RidList};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn escape(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '\r' => out.push_str("\\r"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\\' => out.push_str("\\\\"),
            c if c.is_control()
                || matches!(
                    c,
                    '\u{a0}' | '\u{ad}' | '\u{200b}'..='\u{200f}' | '\u{feff}'
                ) =>
            {
                out.push_str(&format!("\\u{{{:x}}}", c as u32))
            }
            c => out.push(c),
        }
    }
    out
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [path, table_name, value_name] = args.as_slice() else {
        return Err("usage: index_entries <file.fmp12> <table> <value field>".into());
    };
    let file = HbamFile::open(path)?;
    let catalog = file.catalog()?;
    let table = catalog
        .tables
        .iter()
        .find(|t| &t.name == table_name)
        .ok_or("no such table")?;
    let value_field = table
        .fields
        .iter()
        .find(|f| &f.name == value_name)
        .ok_or("no such field")?
        .id;
    let records = file.records(table.id)?;
    let mut values = BTreeMap::new();
    for record in &records {
        let text = record.text(value_field).transpose()?.unwrap_or_default();
        values.insert(record.id, text);
    }
    let table_key = uint_segment(table.id);
    for field in &table.fields {
        let field_key = uint_segment(field.id);
        let mut keys: [BTreeMap<u64, BTreeSet<String>>; 2] = Default::default();
        let mut indexed = false;
        for (kind, by_record) in keys.iter_mut().enumerate() {
            let Some(index) = file.subtree(&[&table_key, &[1], &field_key, &[kind as u8]])? else {
                continue;
            };
            indexed = true;
            for entry in &index.children {
                for id in RidList::from_node(entry)?.iter() {
                    by_record
                        .entry(id)
                        .or_default()
                        .insert(hex(entry.segment.0));
                }
            }
        }
        if !indexed {
            continue;
        }
        let language = field
            .decoded_options()
            .map_or(String::new(), |o| o.index_language().to_string());
        let join = |by_record: &BTreeMap<u64, BTreeSet<String>>, id: u64| {
            by_record
                .get(&id)
                .map(|keys| keys.iter().cloned().collect::<Vec<_>>().join(" "))
                .unwrap_or_default()
        };
        for (id, value) in &values {
            println!(
                "{}\t{language}\t{id}\t{}\t{}\t{}",
                field.name,
                escape(value),
                join(&keys[0], *id),
                join(&keys[1], *id)
            );
        }
    }
    Ok(())
}
