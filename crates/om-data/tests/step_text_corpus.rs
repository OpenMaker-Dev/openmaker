//! Every step of every script in the sample corpus renders as Script
//! Workspace text (`om_scripts::display`) with the file's own names: no
//! panic, every line starts with its step's name (after `// ` when
//! disabled), and every option span lies on the text. Unresolved script,
//! layout and field references are counted for inspection; the test does
//! not assert that none remain.
//!
//! Ignored by default: the samples exist only on the analysis host. Run
//! with
//!   OM_STEP_TEXT_CORPUS=<dir or file>:... \
//!     cargo test --release -p om-data --test step_text_corpus -- --ignored --nocapture
//! (the templates, the FileMaker Server samples and the MBS examples).
//! `OM_STEP_TEXT_REPORT=<file>` writes each step id's distinct texts (up to
//! eight) for checking by eye.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use om_data::{ScriptCatalog, ScriptKind};
use om_hbam::{Catalog, HbamFile};
use om_scripts::display::{script_lines, LineKind, UNKNOWN};

const TEMPLATES: &str =
    "../../re/extracted/mac/FileMaker Pro 26/FileMaker Pro.app/Contents/Resources/DynamicTemplates";
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

#[derive(Default)]
struct Report {
    files: usize,
    scripts: usize,
    steps: usize,
    disabled: usize,
    comments: usize,
    /// Steps whose text names a reference the file does not have.
    unknown: usize,
    /// Distinct texts per step id.
    samples: BTreeMap<u16, BTreeSet<String>>,
}

fn check(path: &Path, report: &mut Report) {
    let file = HbamFile::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let tree = file.key_tree().expect("key tree");
    let catalog = Catalog::from_tree(&tree).expect("catalog");
    let schema = om_data::rules::schema::calc_schema(&tree, &catalog, "corpus");
    let scripts = ScriptCatalog::decode(&tree).expect("scripts");
    report.files += 1;
    for script in scripts
        .scripts
        .iter()
        .filter(|s| s.kind == ScriptKind::Script)
    {
        report.scripts += 1;
        let lines = script_lines(&script.steps, &schema);
        assert_eq!(lines.len(), script.steps.len());
        for (step, line) in script.steps.iter().zip(&lines) {
            let at = format!("{} {} step {}", path.display(), script.name, step.id());
            report.steps += 1;
            let body = match line.text.strip_prefix("// ") {
                Some(body) => {
                    report.disabled += 1;
                    assert!(!step.is_enabled(), "{at}");
                    body
                }
                None => {
                    assert!(step.is_enabled() || step.id() == 0, "{at}");
                    &line.text
                }
            };
            if line.kind == LineKind::Comment {
                report.comments += 1;
                assert!(body.is_empty() || body.starts_with("# "), "{at}: {body}");
            } else {
                assert!(body.starts_with(&step.name()), "{at}: {body}");
            }
            for option in &line.options {
                assert!(line.text.get(option.range.clone()).is_some(), "{at}");
            }
            if body.contains(UNKNOWN) {
                report.unknown += 1;
            }
            let samples = report.samples.entry(step.id()).or_default();
            if samples.len() < 8 {
                samples.insert(body.to_owned());
            }
        }
    }
}

#[test]
#[ignore = "requires the sample corpus"]
fn every_corpus_step_renders() {
    let mut paths = Vec::new();
    files(Path::new(TEMPLATES), &mut paths);
    files(Path::new(FMS_SAMPLES), &mut paths);
    for entry in std::env::var("OM_STEP_TEXT_CORPUS")
        .unwrap_or_default()
        .split(':')
        .filter(|e| !e.is_empty())
    {
        let path = Path::new(entry);
        if path.is_dir() {
            files(path, &mut paths);
        } else if path.is_file() {
            paths.push(path.to_path_buf());
        }
    }
    paths.sort();
    paths.dedup();
    assert!(!paths.is_empty(), "no sample files found");
    let mut report = Report::default();
    for path in &paths {
        check(path, &mut report);
    }
    println!(
        "{} files, {} scripts, {} steps ({} disabled, {} comments), {} with {UNKNOWN} \
         references, {} step ids",
        report.files,
        report.scripts,
        report.steps,
        report.disabled,
        report.comments,
        report.unknown,
        report.samples.len()
    );
    if let Ok(out) = std::env::var("OM_STEP_TEXT_REPORT") {
        let mut text = String::new();
        for (id, samples) in &report.samples {
            for sample in samples {
                text.push_str(&format!("{id}\t{sample}\n"));
            }
        }
        std::fs::write(out, text).expect("report");
    }
}

/// How each corpus step's text came out.
#[derive(Default)]
struct RoundTrip {
    steps: usize,
    display: usize,
    extras: usize,
    raw: usize,
    /// Steps whose text did not read back to the same step (must be 0).
    failed: Vec<String>,
    /// The same texts read as of unknown origin (a paste from elsewhere)
    /// into their own file: the same step, the same step but for dropped
    /// stray reference bytes, or failed (with the reasons' counts).
    unknown_same: usize,
    unknown_reference_dropped: usize,
    unknown_failed: BTreeMap<String, usize>,
    /// Extras and raw reasons, by step id and kind.
    reasons: BTreeMap<(u16, String), usize>,
}

fn round_trip(path: &Path, report: &mut RoundTrip) {
    use om_scripts::display::{
        canonical, parse_steps, step_text_fidelity, steps_text, Fidelity, Origin, Pasted,
    };
    let file = HbamFile::open(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let tree = file.key_tree().expect("key tree");
    let catalog = Catalog::from_tree(&tree).expect("catalog");
    let schema = om_data::rules::schema::calc_schema(&tree, &catalog, "corpus");
    let scripts = ScriptCatalog::decode(&tree).expect("scripts");
    for script in scripts
        .scripts
        .iter()
        .filter(|s| s.kind == ScriptKind::Script)
    {
        // The whole script as text reads back to as many steps.
        let text = steps_text(&script.steps, &schema);
        let pasted =
            parse_steps(&text, &schema, Origin::ThisFile).expect("within the paste limits");
        if pasted.len() != script.steps.len() {
            let merged = pasted
                .iter()
                .find_map(|p| match p {
                    Pasted::Failed { text, .. } if text.contains('\n') => Some(text.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            report.failed.push(format!(
                "{} {}: {} steps read back as {}: {merged}",
                path.display(),
                script.name,
                script.steps.len(),
                pasted.len()
            ));
        }
        for step in &script.steps {
            if std::env::var("OM_STEP_TEXT_STATS").is_ok() {
                let r = step.reserved;
                let filler = |key: &[u8]| -> String {
                    let len = usize::from(key[0]).min(5);
                    key[1 + len..].iter().map(|b| format!("{b:02x}")).collect()
                };
                let kind = if step.has_flag(om_scripts::flag::FIELD_SPEC) {
                    format!("field {} | {}", filler(&r[..6]), filler(&r[6..12]))
                } else if step.has_flag(om_scripts::flag::MEMBER_KEY)
                    || step.has_flag(om_scripts::flag::SCRIPT_KEY)
                {
                    format!(
                        "key {} rest {}",
                        filler(&r[..6]),
                        r[6..]
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect::<String>()
                    )
                } else {
                    String::new()
                };
                if !kind.is_empty() {
                    *report.reasons.entry((0, kind)).or_default() += 1;
                }
            }
            report.steps += 1;
            let (line, fidelity) = step_text_fidelity(step, &schema);
            let kind = match &fidelity {
                Fidelity::Display => {
                    report.display += 1;
                    None
                }
                Fidelity::Extras(options) => {
                    report.extras += 1;
                    Some(
                        options
                            .iter()
                            .map(|o| o.split(':').next().unwrap_or_default().to_owned())
                            .collect::<Vec<_>>()
                            .join(", "),
                    )
                }
                Fidelity::Raw(reasons) => {
                    report.raw += 1;
                    Some(format!(
                        "raw: {}",
                        reasons.first().cloned().unwrap_or_default()
                    ))
                }
            };
            // What keeps the display from being enough, where the line
            // shows it: a plug-in function the file does not name, or a
            // field or table occurrence it no longer has.
            let kind = kind.map(|kind| {
                if line.contains("<Function Missing>") {
                    format!("[plug-in function] {kind}")
                } else if line.contains("<Field Missing>") || line.contains("<Table Missing>") {
                    format!("[missing reference] {kind}")
                } else {
                    kind
                }
            });
            if let Some(kind) = kind {
                let debug = std::env::var("OM_STEP_TEXT_DEBUG").ok();
                let count = report.reasons.entry((step.id(), kind.clone())).or_default();
                *count += 1;
                if debug.as_deref() == Some(&step.id().to_string()) && *count <= 2 {
                    let read = om_scripts::display::display_reading(step, &schema);
                    println!(
                        "DEBUG {kind}\n  line {line}\n  want {:?}\n  read {:?}",
                        canonical(step),
                        read
                    );
                }
            }
            // Each step's own text reads back to the same step.
            // Read as a paste of unknown origin, into its own file.
            match parse_steps(&format!("{line}\n"), &schema, Origin::Unknown)
                .unwrap()
                .as_slice()
            {
                [Pasted::Step(parsed)] if canonical(parsed) == canonical(step) => {
                    report.unknown_same += 1
                }
                [Pasted::Step(parsed)]
                    if canonical(&om_scripts::ScriptStep {
                        reserved: step.reserved,
                        ..parsed.clone()
                    }) == canonical(step) =>
                {
                    report.unknown_reference_dropped += 1
                }
                [Pasted::Failed { reason, .. }] => {
                    let kind = reason.split(" (").next().unwrap_or(reason);
                    let kind = format!("{} {}: {kind}", step.id(), step.name());
                    *report.unknown_failed.entry(kind).or_default() += 1;
                }
                other => {
                    let kind = format!("{} {}: a different step", step.id(), step.name());
                    if std::env::var("OM_STEP_TEXT_DEBUG").as_deref() == Ok("unknown") {
                        println!(
                            "DIFFERENT {line}\n  want {:?}\n  read {other:?}",
                            canonical(step)
                        );
                    }
                    *report.unknown_failed.entry(kind).or_default() += 1
                }
            }
            match parse_steps(&format!("{line}\n"), &schema, Origin::ThisFile)
                .unwrap()
                .as_slice()
            {
                [Pasted::Step(parsed)] if canonical(parsed) == canonical(step) => {}
                [Pasted::Step(_)] => report
                    .failed
                    .push(format!("{}: {line}: different step", path.display())),
                [Pasted::Failed { reason, .. }] => report
                    .failed
                    .push(format!("{}: {line}: {reason}", path.display())),
                other => report.failed.push(format!(
                    "{}: {line}: {} records",
                    path.display(),
                    other.len()
                )),
            }
        }
    }
}

#[test]
#[ignore = "requires the sample corpus"]
fn every_corpus_step_round_trips_through_its_text() {
    let mut paths = Vec::new();
    files(Path::new(TEMPLATES), &mut paths);
    files(Path::new(FMS_SAMPLES), &mut paths);
    for entry in std::env::var("OM_STEP_TEXT_CORPUS")
        .unwrap_or_default()
        .split(':')
        .filter(|e| !e.is_empty())
    {
        let path = Path::new(entry);
        if path.is_dir() {
            files(path, &mut paths);
        } else if path.is_file() {
            paths.push(path.to_path_buf());
        }
    }
    paths.sort();
    paths.dedup();
    assert!(!paths.is_empty(), "no sample files found");
    let mut report = RoundTrip::default();
    for path in &paths {
        round_trip(path, &mut report);
    }
    println!(
        "{} steps: {} by their display text alone, {} with stored options, {} carried whole; {} failed",
        report.steps,
        report.display,
        report.extras,
        report.raw,
        report.failed.len()
    );
    println!(
        "read as of unknown origin: {} the same step, {} the same but for stray reference \
         bytes, {} failed",
        report.unknown_same,
        report.unknown_reference_dropped,
        report.unknown_failed.values().sum::<usize>()
    );
    let mut unknown: Vec<_> = report.unknown_failed.iter().collect();
    unknown.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (kind, n) in &unknown {
        println!("{n:>6}  unknown origin: {kind}");
    }
    let mut reasons: Vec<_> = report.reasons.iter().collect();
    reasons.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for ((id, kind), n) in reasons.iter().take(60) {
        println!(
            "{n:>6}  {id:>3} {}  {kind}",
            om_scripts::step_name(*id).unwrap_or("?")
        );
    }
    if let Ok(out) = std::env::var("OM_STEP_TEXT_REASONS") {
        let text: String = reasons
            .iter()
            .map(|((id, kind), n)| format!("{n}\t{id}\t{kind}\n"))
            .collect();
        std::fs::write(out, text).expect("reasons");
    }
    for failure in report.failed.iter().take(20) {
        println!("FAILED {failure}");
    }
    assert!(
        report.failed.is_empty(),
        "{} steps did not round-trip",
        report.failed.len()
    );
}
