//! Read-only field metadata inventory of a local file (auto-entry.md); it
//! reads no record data.
//! `cargo run -p om-provider --example field_options -- <file.fmp12>`
use om_hbam::{HbamFile, SimpleGetSelector};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let file = HbamFile::open(std::env::args_os().nth(1).ok_or("file required")?)?;
    let tree = file.key_tree()?;
    let catalog = om_hbam::Catalog::from_tree(&tree)?;
    for table in &catalog.tables {
        for field in &table.fields {
            let Some(options) = field.decoded_options() else {
                println!(
                    "{}::{}: unrecognized field definition",
                    table.name, field.name
                );
                continue;
            };
            let code = tree
                .at(&[table.id, 3, 5, field.id, 5, 6])
                .and_then(|node| node.assembled());
            println!(
                "{}::{} id={} kind={:?} type={:?} options={:#010x} validation={:#010x} \
                 index_language={} variable={:?} calculation={} serial={} constant={} \
                 protected={} code_bytes={} simple_get={:?}",
                table.name,
                field.name,
                field.id,
                field.kind,
                field.data_type,
                options.options,
                options.validation_options,
                options.index_language(),
                options.auto_entry_variable(),
                options.auto_entry_calculation(),
                options.auto_entry_serial(),
                options.auto_entry_constant(),
                options.auto_entry_editing_prohibited(),
                code.as_ref().map_or(0, |code| code.len()),
                code.as_ref()
                    .and_then(|code| SimpleGetSelector::decode(code)),
            );
        }
    }
    Ok(())
}
