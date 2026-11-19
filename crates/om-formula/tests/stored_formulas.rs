//! Every formula of FileMaker's shipped templates and the test project:
//! decode, re-encode byte for byte, print, tokenize and compile back
//! (calc-encoding.md 11). The files are copied before they are opened.
//!
//! Ignored by default: the templates come with FileMaker Pro on a Mac, and
//! `OM_TEST_PROJECT` names the test project's `.fmp12`.
//!   cargo test -p om-formula --test stored_formulas -- --ignored
//! `OM_FORMULA_EXTRA_CORPUS` names a directory of further `.fmp12` files
//! (the MBS plug-in examples) for the second test.
mod corpus;

use std::path::{Path, PathBuf};

const TEMPLATES: &str = "/Applications/FileMaker Pro.app/Contents/Resources/DynamicTemplates";

/// Copy `files` into a fresh scratch directory and check them there.
fn check_copies(label: &str, files: &[PathBuf]) -> corpus::Report {
    let scratch = std::env::temp_dir().join(format!("om-formula-{label}-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let mut report = corpus::Report::default();
    for (i, file) in files.iter().enumerate() {
        let copy = scratch.join(format!("{i}.fmp12"));
        std::fs::copy(file, &copy).unwrap();
        let loaded = corpus::load(&copy).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        corpus::check(&loaded, &mut report);
        std::fs::remove_file(&copy).unwrap();
    }
    std::fs::remove_dir(&scratch).ok();
    for failure in &report.failures {
        eprintln!("{failure}");
    }
    eprintln!("files: {}\n{}", files.len(), report.summary());
    report
}

fn fmp12_files(dir: &Path, depth: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() && depth > 0 {
            out.extend(fmp12_files(&path, depth - 1));
        } else if path.extension().is_some_and(|e| e == "fmp12") {
            out.push(path);
        }
    }
    out.sort();
    out
}

#[test]
#[ignore = "requires FileMaker Pro's templates on a Mac and OM_TEST_PROJECT naming the test project"]
fn templates_and_test_project_round_trip() {
    let test_project = std::env::var_os("OM_TEST_PROJECT").expect("set OM_TEST_PROJECT");
    let mut files = fmp12_files(Path::new(TEMPLATES), 1);
    files.push(PathBuf::from(test_project));
    assert_eq!(files.len(), 67);
    let report = check_copies("templates", &files);
    assert_eq!(report.formulas, 5304);
    assert!(report.complete(), "{}", report.summary());
    assert_eq!(report.missing_names, 0);
}

#[test]
#[ignore = "requires OM_FORMULA_EXTRA_CORPUS pointing at a directory of .fmp12 files"]
fn extra_corpus_round_trips() {
    let dir = std::env::var_os("OM_FORMULA_EXTRA_CORPUS").expect("set OM_FORMULA_EXTRA_CORPUS");
    let report = check_copies("extra", &fmp12_files(Path::new(&dir), 0));
    assert_eq!(report.polish_reencoded, report.formulas);
    assert_eq!(report.formatted_reencoded, report.formatted_present);
    assert_eq!(report.catalog_names_exact, report.catalog_names);
    // One third-party formatted stream lacks two separators its Polish code
    // implies (calc-encoding.md 4.3), so neither it nor its text compiles,
    // as in FileMaker; one stores a field as an unresolved name (5.6).
    assert!(report.tokens_compiled + 1 >= report.formatted_present);
    let printable = report.formatted_present - report.missing_names;
    assert!(report.text_compiled + 1 >= printable);
    assert!(report.text_tokenized + 2 >= printable);
}
