//! A probe file for script oracle battery s34-containers-release
//! (`tools/fms-script-harness`, containers.md 12.6): a copy of a patched
//! file whose script `script` has, as step `index` (0-based), a Truncate
//! Table that FMUpgradeTool imported without its table, given base table
//! `table` (hexadecimal, `8002`) in its inline reference with `kMemberKey`
//! (script-steps.md 2.1, 10), written with `om_hbam::writer`. No sample
//! holds the step's Save as XML form, so a patch cannot name the table.
//!
//! ```sh
//! cargo run -p om-server --example step_table_probe -- in.fmp12 out.fmp12 truncate 0 8002
//! ```

use om_hbam::{HbamFile, OwnedNode};

/// The script catalog `[17]`, its members `[17, 05, S]`, a member's name
/// `10` and step list `04` (scripts.md).
const SCRIPTS: u8 = 0x11;
const MEMBERS: u8 = 0x05;
const NAME: u8 = 0x10;
const STEPS: u8 = 0x04;
/// One step descriptor: step key 6 bytes, inline reference 14, step id 2,
/// value 2, flags 4 (script-steps.md 2).
const DESCRIPTOR: usize = 28;
const MEMBER_KEY: u32 = 0x8;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let [_, source, target, script, index, table] = args.as_slice() else {
        return Err(
            "usage: step_table_probe <in.fmp12> <out.fmp12> <script> <step> <table hex>".into(),
        );
    };
    let index: usize = index.parse()?;
    let table: Vec<u8> = (0..table.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&table[i..i + 2], 16))
        .collect::<Result<_, _>>()?;
    let file = HbamFile::open(source)?;
    let mut root = file.key_tree()?.root.to_owned_node();
    let members = root
        .at_mut(&[&[SCRIPTS], &[MEMBERS]])
        .ok_or("no script catalog")?;
    let member: &mut OwnedNode = members
        .children
        .iter_mut()
        .find(|m| {
            m.child(&[NAME])
                .and_then(|n| n.value.as_deref())
                .and_then(|v| om_scsu::decode_scsu(v).ok())
                .as_deref()
                == Some(script.as_str())
        })
        .ok_or("no such script")?;
    let steps = member
        .child_mut(&[STEPS])
        .and_then(|n| n.value.as_mut())
        .ok_or("the script has no step list")?;
    let at = index * DESCRIPTOR;
    let descriptor = steps.get_mut(at..at + DESCRIPTOR).ok_or("no such step")?;
    // `[length][segment length][segment]`, zero padded.
    let mut reserved = [0u8; 14];
    reserved[0] = table.len() as u8 + 1;
    reserved[1] = table.len() as u8;
    reserved[2..2 + table.len()].copy_from_slice(&table);
    descriptor[6..20].copy_from_slice(&reserved);
    let flags = u32::from_be_bytes(descriptor[24..28].try_into()?) | MEMBER_KEY;
    descriptor[24..28].copy_from_slice(&flags.to_be_bytes());
    std::fs::write(
        target,
        om_hbam::writer::rebuild(&root, file.superblock().raw)?,
    )?;
    Ok(())
}
