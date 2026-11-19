//! Print exact formula streams for an oracle-generated script, in step order.
use om_hbam::HbamFile;
use om_scripts::ScriptCatalog;
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let file = HbamFile::open(std::env::args_os().nth(1).ok_or("file required")?)?;
    let tree = file.key_tree()?;
    let catalog = ScriptCatalog::decode(&tree)?;
    let script = catalog
        .scripts
        .iter()
        .find(|s| {
            s.name
                == std::env::args()
                    .nth(2)
                    .unwrap_or_else(|| "OracleCompileGenerated".into())
        })
        .ok_or("generated script missing")?;
    for (index, step) in script.steps.iter().enumerate() {
        let Some(data) = &step.data else {
            continue;
        };
        for (slot, _) in data.slots() {
            let Some(formula) = step.calculation(slot) else {
                continue;
            };
            if formula.formatted.is_some() || formula.polish.is_some() {
                println!(
                    "{}\t{}\t{}\t{}",
                    index,
                    slot,
                    formula.formatted.as_deref().map(hex).unwrap_or_default(),
                    formula.polish.as_deref().map(hex).unwrap_or_default()
                );
            }
        }
    }
    Ok(())
}
