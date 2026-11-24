//! Create the synthetic app used to exercise the client on a device.
#[path = "../tests/support/mod.rs"]
mod support;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("supply a new .fmp12 path")?;
    let path = std::path::Path::new(&path);
    // Reserve the name before the fixture writer opens it.
    std::fs::File::create_new(path)?;
    support::fixture(path);
    Ok(())
}
