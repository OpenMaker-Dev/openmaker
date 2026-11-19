//! Check the formula codec, printer and compiler against `.fmp12` files:
//! every stored formula is decoded and re-encoded byte for byte, printed,
//! tokenized back to its formatted stream and compiled back to its Polish
//! stream (calc-encoding.md 11).
//!
//! `cargo run -p om-formula --example check_corpus -- <file.fmp12>...`
//!
//! With `--print` first, print every formula instead (`file`, `path`,
//! text escaped as `tools/fms-calc-harness` prints FileMaker's display text),
//! to compare with the engine's round-trip mode. With `--compile <file>
//! <context>`, compile the lines of standard input in that file and table
//! occurrence (stored form, `03d00001`) and print the columns of
//! `tools/fms-calc-harness -c`, to compare with the engine's compiler.
//!
//! Open copies of FileMaker's files, not the originals.
#[path = "../tests/corpus/mod.rs"]
mod corpus;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|a| a == "--compile") && args.len() == 3 {
        let file = corpus::load(std::path::Path::new(&args[1]))?;
        let context = args[2].to_string_lossy();
        let out = std::io::stdout();
        corpus::compile_lines(&file, &context, std::io::stdin().lock(), &mut out.lock())?;
        return Ok(());
    }
    if args.first().is_some_and(|a| a == "--print") {
        let out = std::io::stdout();
        let mut out = out.lock();
        for arg in &args[1..] {
            corpus::print_all(&corpus::load(std::path::Path::new(arg))?, &mut out)?;
        }
        return Ok(());
    }
    let mut report = corpus::Report::default();
    let mut files = 0;
    for arg in args.drain(..) {
        let path = std::path::PathBuf::from(arg);
        match corpus::load(&path) {
            Ok(file) => {
                corpus::check(&file, &mut report);
                files += 1;
            }
            Err(e) => eprintln!("{}: {e}", path.display()),
        }
    }
    for failure in &report.failures {
        println!("{failure}");
    }
    println!("files: {files}\n{}", report.summary());
    Ok(())
}
