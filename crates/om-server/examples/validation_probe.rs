//! A probe file for the script oracle's batteries s17 and s19
//! (`tools/fms-script-harness`, script-runtime.md 13.20, 13.22): a copy of the
//! Contacts starter template whose field `rule` names gets three
//! repetitions and one validation rule, written with `om_hbam::writer`.
//!
//! ```sh
//! cargo run -p om-server --example validation_probe -- contacts-en.fmp12 contacts-calc.fmp12 calc
//! ```
//!
//! For battery s18 (script-runtime.md 13.21) `spec` sets any Contacts
//! fields by id: data type (1 text, 2 number, 3 date, 4 time, 5
//! timestamp), validation word and repetitions, and optionally option bits
//! to set (`10000` global storage) and the maximum number of characters, in
//! hexadecimal for the words:
//!
//! ```sh
//! cargo run -p om-server --example validation_probe -- contacts-en.fmp12 contacts-number.fmp12 \
//!     spec 1=2,1404,1 2=2,1404,3 4=2,1000,1 6=2,1000,3
//! ```
//!
//! FileMaker Server's engine only enforces a rule on a field whose
//! definition FileMaker's own schema code wrote (this copy lacks that
//! bookkeeping), so the battery runs a file FMUpgradeTool generates from
//! the copy's XML export (`@trigger` lines make `runbat.sh` do that).

use om_formula::{Key, NameTable};
use om_hbam::slot::uint_segment;
use om_hbam::{Catalog, HbamFile, OwnedNode};

/// Each rule: its name, the Contacts field, the validation word (strict
/// `0x4`, not empty `0x8`, unique `0x10`, by calculation `0x1`, maximum
/// characters `0x200`, Always `0x400`; schema-path.md 5.5), the maximum
/// number of characters and the validation calculation.
const RULES: [(&str, u64, u32, u32, Option<&str>); 20] = [
    ("notempty", 4, 0x40c, 0, None),
    ("calc", 7, 0x405, 0, Some("Length ( Self ) <= 3")),
    ("calcctx", 1, 0x405, 0, Some("not IsEmpty ( Company )")),
    (
        "calcrep",
        1,
        0x405,
        0,
        Some("Get ( CalculationRepetitionNumber ) = 1"),
    ),
    ("calcself", 1, 0x405, 0, Some("Length ( First Name ) <= 3")),
    ("calcentry", 7, 0x005, 0, Some("Length ( Self ) <= 3")),
    ("maxchars", 2, 0x604, 3, None),
    ("unique", 6, 0x414, 0, None),
    ("override", 4, 0x408, 0, None),
    ("overridecalc", 7, 0x401, 0, Some("Length ( Self ) <= 3")),
    ("entryonly", 4, 0x00c, 0, None),
    ("calcempty", 7, 0x405, 0, Some("not IsEmpty ( Self )")),
    // `0x2`: "Validate only if field has been modified" off.
    ("calcunmodified", 7, 0x007, 0, Some("Length ( Self ) <= 3")),
    ("calcerror", 7, 0x405, 0, Some("Length ( Self ) / 0 < 1")),
    (
        "calcunmodifiedempty",
        7,
        0x407,
        0,
        Some("not IsEmpty ( Self )"),
    ),
    // Battery s19: how a non-repeating field (Company) reads while
    // repetition n is validated, named bare, as its repetition 1 or 2,
    // through GetRepetition and Extend.
    ("extbare", 1, 0x405, 0, Some("Self = Company")),
    ("extindex1", 1, 0x405, 0, Some("Self = Company[1]")),
    (
        "extgetrep",
        1,
        0x405,
        0,
        Some("Self = GetRepetition ( Company ; 1 )"),
    ),
    ("extextend", 1, 0x405, 0, Some("Self = Extend ( Company )")),
    ("extindex2", 1, 0x405, 0, Some("IsEmpty ( Company[2] )")),
];

/// One field's new definition: data type, validation word, maximum
/// characters, repetitions, option bits to set and validation calculation.
struct Change<'a> {
    field: u64,
    data_type: Option<u8>,
    validation: u32,
    limit: u32,
    repetitions: u16,
    options: u32,
    formula: Option<&'a str>,
}

/// `<field>=<type>,<validation hex>,<repetitions>[,<options hex>[,<maximum
/// characters>]]` (spec mode).
fn parse_spec(spec: &str) -> Result<Change<'_>, String> {
    let bad = || {
        format!(
            "bad field spec {spec:?}: <field>=<type>,<validation hex>,<repetitions>\
             [,<options hex>[,<maximum characters>]]"
        )
    };
    let (field, rest) = spec.split_once('=').ok_or_else(bad)?;
    let parts: Vec<&str> = rest.split(',').collect();
    let (data_type, validation, repetitions, options, limit) = match parts.as_slice() {
        [t, v, r] => (t, v, r, "0", "0"),
        [t, v, r, o] => (t, v, r, *o, "0"),
        [t, v, r, o, l] => (t, v, r, *o, *l),
        _ => return Err(bad()),
    };
    Ok(Change {
        field: field.parse().map_err(|_| bad())?,
        data_type: Some(data_type.parse().map_err(|_| bad())?),
        validation: u32::from_str_radix(validation, 16).map_err(|_| bad())?,
        limit: limit.parse().map_err(|_| bad())?,
        repetitions: repetitions.parse().map_err(|_| bad())?,
        options: u32::from_str_radix(options, 16).map_err(|_| bad())?,
        formula: None,
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let (source, target, changes) = match args.as_slice() {
        [_, source, target, mode, specs @ ..] if mode == "spec" && !specs.is_empty() => (
            source,
            target,
            specs
                .iter()
                .map(|s| parse_spec(s))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        [_, source, target, rule] => {
            let &(_, field, validation, limit, formula) = RULES
                .iter()
                .find(|r| r.0 == rule)
                .ok_or_else(|| format!("unknown rule {rule}"))?;
            let change = Change {
                field,
                data_type: None,
                validation,
                limit,
                repetitions: 3,
                options: 0,
                formula,
            };
            (source, target, vec![change])
        }
        _ => {
            return Err(
                "usage: validation_probe <contacts-en.fmp12> <out.fmp12> <rule> | \
                        spec <field>=<type>,<validation hex>,<repetitions>..."
                    .into(),
            )
        }
    };
    let file = HbamFile::open(source)?;
    let tree = file.key_tree()?;
    let catalog = Catalog::from_tree(&tree)?;
    let mut names = NameTable::default();
    let mut contacts = None;
    for occurrence in &catalog.table_occurrences {
        if let Some(table) = occurrence.base_table() {
            names.add_occurrence(occurrence.id, &occurrence.name, table);
            if occurrence.name == "Contacts" {
                contacts = Some((occurrence.id, table));
            }
        }
    }
    for table in &catalog.tables {
        for f in &table.fields {
            names.add_field(table.id, f.id, &f.name);
        }
    }
    let (occurrence, table) = contacts.ok_or("no Contacts occurrence")?;
    let mut root = tree.root.to_owned_node();
    for change in changes {
        let (t, f) = (uint_segment(table), uint_segment(change.field));
        // The 26-byte definition: data type at 1, option word at 8,
        // validation word at 12, maximum characters at 20, repetitions at 24
        // (schema-path.md 5.5).
        let path: [&[u8]; 5] = [&t, &[3], &[5], &f, &[2]];
        let mut definition = root
            .at(&path)
            .and_then(|n| n.value.clone())
            .ok_or("no field definition")?;
        if let Some(data_type) = change.data_type {
            definition[1] = data_type;
        }
        let options = u32::from_be_bytes(definition[8..12].try_into().expect("4 bytes"));
        definition[8..12].copy_from_slice(&(options | change.options).to_be_bytes());
        definition[12..16].copy_from_slice(&change.validation.to_be_bytes());
        definition[20..24].copy_from_slice(&change.limit.to_be_bytes());
        definition[24..26].copy_from_slice(&change.repetitions.to_be_bytes());
        root.put(&path, OwnedNode::leaf(Vec::new(), definition));
        if let Some(text) = change.formula {
            let mut ctx = om_formula::Context::new(&names);
            ctx.occurrence = Some(occurrence);
            ctx.support_self = true;
            let compiled = om_formula::compile(text, &ctx).map_err(|e| format!("{text}: {e:?}"))?;
            // The validation calculation `[T, 3, 5, F, 19]`: context,
            // formatted and Polish streams.
            for (child, bytes) in [
                (4u8, Key::from_id(occurrence).0),
                (5, om_formula::formatted::encode(&compiled.tokens)),
                (6, om_formula::polish::encode(&compiled.expr)),
            ] {
                root.put(
                    &[&t, &[3], &[5], &f, &[0x19], &[child]],
                    OwnedNode::leaf(Vec::new(), bytes),
                );
            }
        }
    }
    std::fs::write(
        target,
        om_hbam::writer::rebuild(&root, file.superblock().raw)?,
    )?;
    Ok(())
}
