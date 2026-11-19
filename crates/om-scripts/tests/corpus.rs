//! Every script in the shipped templates, the FileMaker Server samples and
//! any extra corpus decodes, re-encodes byte for byte, nests, and keeps its
//! cached block indexes under recomputation. The shipped files are also fully
//! typed by the parameter schema. `examples/step_params.rs` prints the
//! detailed coverage.
//!
//! Ignored by default: the samples are git-ignored extractions that exist
//! only on the analysis host. Run with
//!   cargo test -p om-scripts --test corpus -- --ignored
//! `OM_SCRIPT_CORPUS` adds files or directories (`:`-separated); for those,
//! unclassified items are reported but not failed.
use om_hbam::HbamFile;
use om_scripts::{Script, ScriptCatalog};
use std::path::{Path, PathBuf};

const TEMPLATES: [&str; 2] = [
    "../../re/extracted/mac/FileMaker Pro 26/FileMaker Pro.app/Contents/Resources/DynamicTemplates",
    "/Applications/FileMaker Pro.app/Contents/Resources/DynamicTemplates",
];
const FMS_SAMPLES: &str =
    "../../re/extracted/fms/deb/opt/FileMaker/FileMaker Server/Data/Databases/.Sample";

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for path in entries.flatten().map(|e| e.path()) {
        if path.is_dir() {
            files(&path, out);
        } else if path.extension().is_some_and(|e| e == "fmp12")
            || path.to_string_lossy().ends_with("FMServer_Sample")
        {
            out.push(path);
        }
    }
}

/// Check one file; returns (definitions, steps, unclassified steps).
fn check(path: &Path) -> (usize, usize, usize) {
    let file = HbamFile::open(path).expect("readable file");
    let tree = file.key_tree().expect("key tree");
    ScriptCatalog::decode(&tree).unwrap_or_else(|e| panic!("{path:?}: {}", e.0));
    let (mut definitions, mut steps, mut partial) = (0, 0, 0);
    let Some(node) = tree.at(&[17, 5]) else {
        return (0, 0, 0);
    };
    for node in node
        .children
        .iter()
        .filter(|n| n.id().is_some_and(|id| id != 0))
    {
        let script = Script::decode(node).expect("decodes");
        let at = format!("{path:?} {}", script.name);
        assert_eq!(script.encode(), node.to_owned_node(), "{at}: re-encode");
        script
            .check_structure()
            .unwrap_or_else(|e| panic!("{at}: {}", e.0));
        let mut recomputed = script.clone();
        recomputed.update_partner_indexes().expect("nests");
        assert_eq!(recomputed, script, "{at}: cached indexes");
        definitions += 1;
        steps += script.steps.len();
        partial += script
            .steps
            .iter()
            .filter(|s| !s.unclassified().is_empty())
            .count();
    }
    (definitions, steps, partial)
}

#[test]
#[ignore = "requires the git-ignored templates and FileMaker Server samples"]
fn every_sample_script_round_trips_and_is_typed() {
    let mut shipped = Vec::new();
    if let Some(dir) = TEMPLATES.iter().map(Path::new).find(|d| d.is_dir()) {
        files(dir, &mut shipped);
    }
    files(Path::new(FMS_SAMPLES), &mut shipped);
    assert!(!shipped.is_empty(), "no sample files found");
    let (mut definitions, mut steps) = (0, 0);
    for path in &shipped {
        let (d, s, partial) = check(path);
        assert_eq!(partial, 0, "{path:?}: steps left unclassified");
        (definitions, steps) = (definitions + d, steps + s);
    }
    let mut extra = Vec::new();
    for entry in std::env::var("OM_SCRIPT_CORPUS")
        .unwrap_or_default()
        .split(':')
    {
        let path = Path::new(entry);
        if path.is_dir() {
            files(path, &mut extra);
        } else if path.is_file() {
            extra.push(path.to_path_buf());
        }
    }
    let mut unclassified = 0;
    for path in &extra {
        let (d, s, partial) = check(path);
        (definitions, steps, unclassified) = (definitions + d, steps + s, unclassified + partial);
    }
    println!(
        "{} files, {definitions} definitions, {steps} steps; {unclassified} extra-corpus steps partially typed",
        shipped.len() + extra.len()
    );
}

/// Every script of the shipped templates runs to its end against the memory
/// host (copies of the files are opened), under FileMaker's rule for steps
/// the host lacks (error 3, go on): no panic, no step budget. The detailed
/// report is `examples/run_scripts.rs`.
#[test]
#[ignore = "requires the shipped templates"]
fn every_template_script_runs_against_the_memory_host() {
    use om_scripts::runtime::{MemoryScriptHost, Options, RunState, Runtime, ScriptCall};
    let mut shipped = Vec::new();
    if let Some(dir) = TEMPLATES.iter().map(Path::new).find(|d| d.is_dir()) {
        files(dir, &mut shipped);
    }
    assert!(!shipped.is_empty(), "no templates found");
    let scratch = std::env::temp_dir().join(format!("om-corpus-run-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).expect("scratch directory");
    let mut scripts = 0;
    for (n, path) in shipped.iter().enumerate() {
        let copy = scratch.join(format!("{n}.fmp12"));
        std::fs::copy(path, &copy).expect("copy");
        let file = HbamFile::open(&copy).expect("readable file");
        let tree = file.key_tree().expect("key tree");
        let catalog = ScriptCatalog::decode(&tree).expect("catalog");
        let mut base = MemoryScriptHost::en_us();
        base.add_catalog(0, &catalog);
        for script in catalog
            .scripts
            .iter()
            .filter(|s| s.kind == om_scripts::ScriptKind::Script)
        {
            let mut host = base.clone();
            let mut runtime = Runtime::new(Options {
                step_budget: Some(100_000),
                ..Options::default()
            });
            runtime
                .perform_script(&mut host, ScriptCall::new(0, script.id))
                .expect("starts");
            let state = runtime.run(&mut host);
            assert_eq!(state, RunState::Idle, "{path:?} {}", script.name);
            scripts += 1;
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    println!(
        "{} files, {scripts} scripts ran to their end",
        shipped.len()
    );
}
