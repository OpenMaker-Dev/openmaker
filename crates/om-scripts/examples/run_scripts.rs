//! Run every script of local files against the memory host and report how
//! they end: completed, stopped at a step the host does not implement (by
//! step), the step budget, or a panic. Files are copied to a temporary
//! directory before they are opened.
//!
//! `cargo run --release -p om-scripts --example run_scripts -- <file.fmp12 | dir>...`
//!
//! Each script runs twice in a fresh session with three empty records per
//! table: once stopping at the first step the memory host lacks, once with
//! FileMaker's rule for a missing handler (error 3, go on). The session has
//! the file's tables, fields, occurrences and layouts, but no record data.

use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use om_calc::eval::FieldInfo;
use om_calc::ValueType;
use om_hbam::{DataType, HbamFile};
use om_scripts::runtime::{
    Accounts, Dialogs, Fields, Files, FoundSets, MemoryScriptHost, Navigation, Network, Options,
    Platform, Records, RunState, Runtime, ScriptCall, ScriptHost, Stop, Windows,
};
use om_scripts::{Script, ScriptCatalog, ScriptKind};
use std::sync::Arc;

/// A host with no capability: calculations and the script catalog only.
struct Bare {
    calc: om_calc::eval::NullHost,
    scripts: Vec<Arc<Script>>,
}

impl om_calc::eval::CalcHost for Bare {
    fn locale(&self) -> &om_calc::Locale {
        self.calc.locale()
    }
    fn today(&self) -> om_calc::Date {
        self.calc.today()
    }
    fn variables(&mut self, scope: om_calc::eval::VarScope) -> &mut om_calc::eval::Variables {
        self.calc.variables(scope)
    }
}

impl ScriptHost for Bare {
    fn script(&mut self, _file: u64, id: u64) -> Option<Arc<Script>> {
        self.scripts.iter().find(|s| s.id == id).cloned()
    }
}
impl Navigation for Bare {}
impl Records for Bare {}
impl Fields for Bare {}
impl FoundSets for Bare {}
impl Windows for Bare {}
impl Dialogs for Bare {}
impl Files for Bare {}
impl Network for Bare {}
impl Accounts for Bare {}
impl Platform for Bare {}

const BUDGET: u64 = 200_000;

#[derive(Default)]
struct Report {
    files: usize,
    scripts: usize,
    /// Enabled steps stored in the scripts.
    stored_steps: usize,
    steps: u64,
    completed: usize,
    unsupported: BTreeMap<String, usize>,
    budget: Vec<String>,
    other: Vec<String>,
    panics: Vec<String>,
    /// FileMaker-rule pass.
    completed_fm: usize,
    alerts: BTreeMap<i32, usize>,
    unknown_steps: BTreeMap<u16, usize>,
    /// Bare-host pass.
    completed_bare: usize,
    bare: BTreeMap<String, usize>,
}

fn files(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            files(&path, out);
        }
    } else if path.extension().is_some_and(|e| e == "fmp12") {
        out.push(path.to_owned());
    }
}

/// The file's schema as a memory session.
fn session(tree: &om_hbam::KeyTree<'_>, catalog: &ScriptCatalog) -> MemoryScriptHost {
    let mut host = MemoryScriptHost::en_us();
    host.add_catalog(0, catalog);
    let Ok(schema) = om_hbam::Catalog::from_tree(tree) else {
        return host;
    };
    for table in &schema.tables {
        host.add_table(table.id, &table.name);
        for field in &table.fields {
            let ty = match field.data_type {
                DataType::Number => ValueType::Number,
                DataType::Date => ValueType::Date,
                DataType::Time => ValueType::Time,
                DataType::Timestamp => ValueType::Timestamp,
                DataType::Container => ValueType::Container,
                _ => ValueType::Text,
            };
            host.add_field(table.id, field.id, &field.name, FieldInfo::stored(ty, 1));
        }
        for record in 1..=3 {
            host.add_record(table.id, record);
        }
    }
    for occurrence in &schema.table_occurrences {
        if let Some(table) = occurrence.base_table() {
            host.add_occurrence(occurrence.id, &occurrence.name, table);
        }
    }
    let first = schema.table_occurrences.first().map_or(0, |o| o.id);
    for layout in &schema.layouts {
        let occurrence = layout
            .layout_def
            .as_deref()
            .and_then(|d| om_layout::LayoutDef::decode(d).ok())
            .and_then(|d| om_layout::occurrence_id(d.table_alias_key))
            .filter(|k| *k != 0)
            .unwrap_or(first);
        host.add_layout(layout.id, &layout.name, occurrence);
    }
    host
}

fn run(
    base: &MemoryScriptHost,
    id: u64,
    options: Options,
) -> (RunState, Runtime, MemoryScriptHost) {
    let mut host = base.clone();
    let mut runtime = Runtime::new(options);
    let state = match runtime.perform_script(&mut host, ScriptCall::new(0, id)) {
        Ok(()) => runtime.run(&mut host),
        Err(code) => RunState::Stopped(Stop::Budget(code.0 as u64)),
    };
    (state, runtime, host)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut paths = Vec::new();
    for arg in std::env::args_os().skip(1) {
        files(Path::new(&arg), &mut paths);
    }
    if paths.is_empty() {
        return Err("give .fmp12 files or directories".into());
    }
    let scratch = std::env::temp_dir().join(format!("om-run-scripts-{}", std::process::id()));
    std::fs::create_dir_all(&scratch)?;
    let mut report = Report::default();
    for (n, path) in paths.iter().enumerate() {
        let copy = scratch.join(format!("{n}.fmp12"));
        std::fs::copy(path, &copy)?;
        let file = HbamFile::open(&copy)?;
        let tree = file.key_tree()?;
        let catalog = ScriptCatalog::decode(&tree).map_err(|e| e.0)?;
        let base = session(&tree, &catalog);
        report.files += 1;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        for script in catalog
            .scripts
            .iter()
            .filter(|s| s.kind == ScriptKind::Script)
        {
            report.scripts += 1;
            report.stored_steps += script.steps.iter().filter(|s| s.is_enabled()).count();
            for step in &script.steps {
                if step.is_enabled()
                    && (!om_scripts::is_valid_step_id(step.id())
                        || om_scripts::OBSOLETE_STEP_IDS.contains(&step.id()))
                {
                    *report.unknown_steps.entry(step.id()).or_default() += 1;
                }
            }
            let at = format!("{name}: {}", script.name);
            let stopping = Options {
                stop_on_unsupported: true,
                step_budget: Some(BUDGET),
                ..Options::default()
            };
            let outcome = catch_unwind(AssertUnwindSafe(|| run(&base, script.id, stopping)));
            let Ok((state, runtime, _)) = outcome else {
                report.panics.push(at);
                continue;
            };
            report.steps += runtime.steps_run();
            match state {
                RunState::Idle => report.completed += 1,
                RunState::Stopped(Stop::Unsupported(location)) => {
                    *report
                        .unsupported
                        .entry(format!("{} ({})", location.step_name, location.step))
                        .or_default() += 1;
                }
                RunState::Stopped(Stop::Budget(_)) => report.budget.push(at.clone()),
                other => report.other.push(format!("{at}: {other:?}")),
            }
            let filemaker = Options {
                step_budget: Some(BUDGET),
                ..Options::default()
            };
            let outcome = catch_unwind(AssertUnwindSafe(|| run(&base, script.id, filemaker)));
            let Ok((state, _, host)) = outcome else {
                report.panics.push(format!("{at} (FileMaker rule)"));
                continue;
            };
            if state == RunState::Idle {
                report.completed_fm += 1;
            }
            let mut bare = Bare {
                calc: om_calc::eval::NullHost::new(base.calc.locale.clone(), base.calc.today),
                scripts: catalog.scripts.iter().cloned().map(Arc::new).collect(),
            };
            let mut runtime = Runtime::new(Options {
                stop_on_unsupported: true,
                step_budget: Some(BUDGET),
                ..Options::default()
            });
            let state = catch_unwind(AssertUnwindSafe(|| {
                runtime
                    .perform_script(&mut bare, ScriptCall::new(0, script.id))
                    .ok()
                    .map(|()| runtime.run(&mut bare))
            }));
            match state {
                Ok(Some(RunState::Idle)) => report.completed_bare += 1,
                Ok(Some(RunState::Stopped(Stop::Unsupported(location)))) => {
                    *report
                        .bare
                        .entry(format!("{} ({})", location.step_name, location.step))
                        .or_default() += 1;
                }
                Ok(_) => {}
                Err(_) => report.panics.push(format!("{at} (bare host)")),
            }
            for alert in &host.alerts {
                *report.alerts.entry(alert.code.0).or_default() += 1;
                if std::env::var_os("OM_RUN_SCRIPTS_VERBOSE").is_some() {
                    eprintln!(
                        "{at}: line {} {}: {}",
                        alert.line, alert.step_name, alert.code
                    );
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    println!(
        "files {}, scripts {} ({} enabled steps stored), steps executed {} (stopping pass)",
        report.files, report.scripts, report.stored_steps, report.steps
    );
    println!(
        "stopping at the first unimplemented host step: completed {}, stopped {}, step budget {}, other {}, panics {}",
        report.completed,
        report.unsupported.values().sum::<usize>(),
        report.budget.len(),
        report.other.len(),
        report.panics.len()
    );
    let mut by_count: Vec<_> = report.unsupported.iter().collect();
    by_count.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    for (step, count) in by_count {
        println!("  {count:5}  {step}");
    }
    println!(
        "FileMaker rule (error 3 and go on): completed {} of {}",
        report.completed_fm, report.scripts
    );
    println!(
        "a host without capabilities: completed {}, stopped {} at:",
        report.completed_bare,
        report.bare.values().sum::<usize>()
    );
    let mut by_count: Vec<_> = report.bare.iter().collect();
    by_count.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    for (step, count) in by_count {
        println!("  {count:5}  {step}");
    }
    println!("errors alerted (capture off), by code:");
    for (code, count) in &report.alerts {
        let message = om_scripts::runtime::ErrorCode(*code)
            .message()
            .unwrap_or("");
        println!("  {count:5}  {code} {message}");
    }
    println!(
        "obsolete or invalid step ids in enabled steps: {:?}",
        report.unknown_steps
    );
    for line in report
        .budget
        .iter()
        .chain(&report.other)
        .chain(&report.panics)
    {
        println!("  {line}");
    }
    Ok(())
}
