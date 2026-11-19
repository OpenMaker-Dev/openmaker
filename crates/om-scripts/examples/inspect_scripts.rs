//! Read-only script inventory of a local file, checking that every script
//! definition re-encodes to its stored nodes.
//! `cargo run -p om-scripts --example inspect_scripts -- <file.fmp12>`
use om_hbam::HbamFile;
use om_scripts::ScriptCatalog;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let file = HbamFile::open(std::env::args_os().nth(1).ok_or("path required")?)?;
    let tree = file.key_tree()?;
    let scripts = ScriptCatalog::decode(&tree)?;
    for script in &scripts.scripts {
        if let Some(original) = tree.at(&[17, 5, script.id]) {
            assert_eq!(script.encode(), original.to_owned_node());
        }
        println!(
            "{} {} {:?}: {} steps",
            script.id,
            script.name,
            script.kind,
            script.steps.len()
        );
    }
    Ok(())
}
