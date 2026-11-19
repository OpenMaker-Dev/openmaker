//! Make a blank file, as File > Create New... does (`om_data::new_file::
//! create`), for checking it in FileMaker and in OpenMaker. `.fmp12` is
//! added to a path without it; an existing file is never replaced.
//!
//! `cargo run -p om-data --example new_file -- <path/Name.fmp12>`
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(std::env::args_os().nth(1).ok_or("missing path")?);
    let created = om_data::new_file::create(&path)?;
    println!(
        "made {} (open it as {} with no password)",
        created.path.display(),
        created.account
    );
    Ok(())
}
