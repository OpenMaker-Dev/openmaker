//! The runtime against FileMaker Server's own script engine: every run of
//! the script oracle's batteries (`tools/fms-script-harness`, results in
//! `results/`) is replayed on `om_scripts::runtime::Runtime` as a server
//! client over the probe file in memory, and its result, error, final
//! `Get ( LastError )`, the run line's expressions, the trace of every
//! executed step (its error, whether it was a control step, `Get (
//! LastError )` after it), the event log and the `@call`s must match the
//! engine's. The probe scripts' block indexes must match what FileMaker's
//! `ScanScript` wrote. Differences that remain are listed in [`KNOWN`] with
//! their reason (script-runtime.md 13 and its implementation notes).

mod support;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use om_calc::eval::CalcHost;
use om_calc::{Ctx, Value};
use om_scripts::runtime::{
    MemoryScriptHost, Options, QueueMode, RunState, Runtime, ScriptCall, ScriptHost, TriggerEvent,
};
use om_scripts::{Client, ScriptKind};
use support::battery::{self, CallSpec, Catalog, RunLine};
use support::contacts;
use support::results::{self, Event, Run, Step};

/// Probe scripts get ids from 1000, as `make_patch.py` gives them.
const FIRST_PROBE: u64 = 1000;

/// Runs that differ from the engine on purpose or for a reason outside the
/// runtime: (battery, 1-based run, script, reason).
const KNOWN: &[(&str, usize, &str, &str)] = &[
    (
        "s10-fms-sample-triggers",
        1,
        "Show All Task List | Web",
        FMS_SAMPLE,
    ),
    (
        "s10-fms-sample-triggers",
        2,
        "Trigger | On First Window Open",
        FMS_SAMPLE,
    ),
    ("s09-step-ids", 29, "srv step 248", STEP_248),
];

/// Why step 248 differs: FileMaker Server 26.0.x, which ran s09, refuses
/// ids from 248 with error 4; OpenMaker follows FileMaker Pro 26.1.1, whose
/// valid ids run to 250 with 248 left unnamed, and fails a valid step
/// without a handler with error 3 [I] (script-runtime.md 2.5, #338).
const STEP_248: &str = "FileMaker Server 26.0.x refused it as an invalid id (error 4); under \
    26.1.1's valid ids it is a step without a handler (error 3)";

/// Why s10 is not replayed.
const FMS_SAMPLE: &str = "runs the FileMaker Server sample's own scripts over its Tasks \
    records, layouts and layout and file triggers, which are not reproduced in memory; its \
    file triggers (OnFirstWindowOpen before the request, OnWindowClose after it) are the \
    server host's to fire around a request, not the runtime's (script-runtime.md 13.1)";

fn harness() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tools/fms-script-harness")
}

/// Replay a battery; panics with every difference not in [`KNOWN`].
fn check(name: &str, file: &str) {
    let dir = harness();
    let text = std::fs::read_to_string(dir.join("batteries").join(format!("{name}.txt")))
        .expect("battery");
    let engine = std::fs::read_to_string(dir.join("results").join(format!("{name}.{file}.tsv")))
        .expect("results");
    let battery = battery::parse(&text);
    let engine = results::parse(&engine);

    let mut host = contacts::host();
    let mut failures = String::new();
    let mut scripts: Vec<(String, u64)> = battery
        .probes
        .iter()
        .enumerate()
        .map(|(i, p)| (p.name.clone(), FIRST_PROBE + i as u64))
        .collect();
    scripts.push(("Sort by Name".into(), contacts::SORT_BY_NAME));
    let probes: Vec<_> = {
        let catalog = Catalog {
            names: host.calc.names(),
            scripts: scripts.clone(),
            layouts: contacts::layouts(&host),
        };
        battery
            .probes
            .iter()
            .enumerate()
            .map(|(i, p)| catalog.script_of(p, FIRST_PROBE + i as u64))
            .collect()
    };
    for scan in &engine.scans {
        let Some(script) = probes.iter().find(|s| s.name == scan.script) else {
            writeln!(failures, "scan of an unknown script {:?}", scan.script).unwrap();
            continue;
        };
        let ours: Vec<(i16, u16)> = script.steps.iter().map(|s| (s.opcode, s.value)).collect();
        if ours != scan.steps {
            writeln!(
                failures,
                "block indexes of {:?}:\n  engine {:?}\n  ours   {:?}",
                scan.script, scan.steps, ours
            )
            .unwrap();
        }
    }
    for script in probes {
        host.add_script(0, script);
    }
    // The generated probe file's triggers: a layout named by `@trigger`
    // has exactly those.
    let layouts = contacts::layouts(&host);
    let layout_id = |name: &str| {
        layouts
            .iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("unknown layout {name:?}"))
            .1
    };
    for trigger in &battery.triggers {
        host.layout_triggers
            .retain(|(layout, _), _| *layout != layout_id(&trigger.layout));
    }
    for trigger in &battery.triggers {
        assert!(
            matches!(trigger.modes.as_str(), "" | "B") && trigger.parameter.is_empty(),
            "the memory host's layout triggers run in Browse mode without a parameter"
        );
        let event = TriggerEvent::from_id(trigger.event_id()).expect("event");
        let script = scripts
            .iter()
            .find(|(n, _)| *n == trigger.script)
            .unwrap_or_else(|| panic!("unknown script {:?}", trigger.script))
            .1;
        host.layout_triggers
            .insert((layout_id(&trigger.layout), event), (0, script));
    }

    let known: BTreeMap<usize, &str> = KNOWN
        .iter()
        .filter(|k| k.0 == name)
        .map(|k| (k.1, k.2))
        .collect();
    let mut calls: Vec<CallSpec> = Vec::new();
    let mut ours_runs = Vec::new();
    for line in &battery.runs {
        match line {
            RunLine::Call(Some(call)) => calls.push(call.clone()),
            RunLine::Call(None) => calls.clear(),
            RunLine::Watch(w) => assert!(w.is_empty(), "@watch is not replayed"),
            RunLine::Run {
                script,
                parameter,
                expressions,
            } => ours_runs.push(run(&mut host, script, parameter, expressions, &calls)),
        }
    }
    assert_eq!(
        ours_runs.len(),
        engine.runs.len(),
        "{name}: run lines and engine runs"
    );
    let (mut matched, mut listed) = (0, 0);
    for (i, (ours, theirs)) in ours_runs.iter().zip(&engine.runs).enumerate() {
        let n = i + 1;
        let diff = differences(ours, theirs);
        match (diff.is_empty(), known.get(&n)) {
            (true, None) => matched += 1,
            (true, Some(_)) => writeln!(
                failures,
                "run {n} ({}) matches now; drop it from KNOWN",
                theirs.script
            )
            .unwrap(),
            (false, Some(script)) if *script == theirs.script => listed += 1,
            (false, Some(script)) => writeln!(
                failures,
                "run {n} is {}, not {script} as KNOWN says",
                theirs.script
            )
            .unwrap(),
            (false, None) => {
                writeln!(
                    failures,
                    "run {n}: {} [{}]",
                    theirs.script, theirs.parameter
                )
                .unwrap();
                failures.push_str(&diff);
            }
        }
    }
    eprintln!(
        "{name}: {matched} of {} runs match, {listed} listed differences",
        engine.runs.len()
    );
    assert!(failures.is_empty(), "{name}:\n{failures}");
}

/// One run line as `RunScriptAndClose` runs it: a new session (runtime,
/// window on the default layout; records stay), the script to the end with
/// the `@call`s, then the expressions.
fn run(
    host: &mut MemoryScriptHost,
    script: &str,
    parameter: &str,
    expressions: &[String],
    calls: &[CallSpec],
) -> Run {
    contacts::open_window(host);
    host.log.clear();
    let mut runtime = Runtime::new(Options::new(Client::Server));
    runtime.set_tracing(true);
    let mut out = Run {
        script: script.into(),
        parameter: parameter.into(),
        ..Run::default()
    };
    let Some(id) = host.script_named(0, script) else {
        // `ExecuteScript`: 104 for a script that is not found.
        out.error = 104;
        return out;
    };
    let mut call = ScriptCall::new(0, id);
    if !parameter.is_empty() {
        call.parameter = Some(Value::text(parameter));
    }
    if let Err(code) = runtime.perform_script(host, call) {
        out.error = code.0;
        return out;
    }
    let mut logged = 0;
    loop {
        let state = runtime.step(host);
        for entry in &host.log[logged..] {
            out.events.push(Event::Log {
                code: entry.code.0,
                message: format!(
                    "{} : {} : {} : {}",
                    contacts::FILE_NAME,
                    entry.script,
                    entry.line,
                    entry.step_name
                ),
            });
        }
        logged = host.log.len();
        let trace = runtime.take_trace();
        for step in &trace {
            out.events.push(Event::Step(Step {
                depth: step.depth,
                script: step.script.clone(),
                line: step.line,
                step: step.step,
                error: step.error.0,
                control: step.control,
                last_error: step.last_error.0,
                watch: Vec::new(),
            }));
        }
        match state {
            RunState::Idle => break,
            RunState::Yielded => {}
            // A server pause is short (0.1 s when indefinite): the script
            // goes on by itself.
            RunState::Paused { .. } => runtime.resume(),
            other => panic!("{script}: the runtime returned {other:?}"),
        }
        let Some(last) = trace.last() else {
            continue;
        };
        for call in calls
            .iter()
            .filter(|c| c.script == last.script && c.line == last.line)
        {
            let target = host.script_named(0, &call.target).expect("@call target");
            let started = runtime.perform_script(
                host,
                ScriptCall {
                    mode: QueueMode::from_value(call.state),
                    queue_at_bottom: call.bottom,
                    ..ScriptCall::new(0, target)
                },
            );
            out.events.push(Event::Call(format!(
                "call {} state {} bottom {} -> error {}\tdepth {}",
                call.target,
                call.state,
                u8::from(call.bottom),
                started.err().map_or(0, |e| e.0),
                runtime.script_stack().len()
            )));
        }
    }
    let cx_locale = host.calc.locale.clone();
    let cx = Ctx::new(&cx_locale, host.calc.today);
    out.error = runtime.final_error().0;
    out.result = runtime
        .final_result()
        .map(|v| v.to_text(&cx).into_string())
        .unwrap_or_default();
    out.last_error = runtime.last_error().0;
    let names: Vec<(String, u64)> = Vec::new();
    for expression in expressions {
        let calc = Catalog {
            names: host.calc.names(),
            scripts: names.clone(),
            layouts: Vec::new(),
        }
        .calc(expression);
        let value = runtime.evaluate(host, &calc).value;
        out.values.push(value.to_text(&cx).into_string());
    }
    out
}

/// What differs between our run and the engine's, as readable lines.
fn differences(ours: &Run, theirs: &Run) -> String {
    let mut out = String::new();
    let mut field = |what: &str, a: &dyn std::fmt::Debug, b: &dyn std::fmt::Debug| {
        let (a, b) = (format!("{a:?}"), format!("{b:?}"));
        if a != b {
            writeln!(out, "  {what}: engine {b}, ours {a}").unwrap();
        }
    };
    field("error", &ours.error, &theirs.error);
    field("result", &ours.result, &theirs.result);
    field("LastError", &ours.last_error, &theirs.last_error);
    field("values", &ours.values, &theirs.values);
    let normalize = |events: &[Event]| -> Vec<String> {
        events
            .iter()
            .map(|e| match e {
                Event::Step(s) => format!(
                    ">\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    s.depth,
                    s.script,
                    s.line,
                    s.step,
                    s.error,
                    u8::from(s.control),
                    s.last_error
                ),
                Event::Log { code, message } => {
                    // Steps without a name: the engine's "<unknown> [id]",
                    // our "Unsupported step (id)".
                    let message = match message
                        .rsplit_once(" : <unknown>")
                        .or_else(|| message.rsplit_once(" : Unsupported step ("))
                    {
                        Some((head, _)) => format!("{head} : <unknown>"),
                        None => message.clone(),
                    };
                    format!("!\t{code}\t{message}")
                }
                Event::Call(text) => format!("@\t{text}"),
            })
            .collect()
    };
    let (a, b) = (normalize(&ours.events), normalize(&theirs.events));
    if a != b {
        let first = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
        writeln!(out, "  events differ from #{}:", first + 1).unwrap();
        for line in b.iter().skip(first).take(6) {
            writeln!(out, "    engine {line}").unwrap();
        }
        for line in a.iter().skip(first).take(6) {
            writeln!(out, "    ours   {line}").unwrap();
        }
    }
    out
}

#[test]
fn s00_smoke() {
    check("s00-smoke", "contacts-en");
}

#[test]
fn s01_lasterror() {
    check("s01-lasterror", "contacts-en");
}

#[test]
fn s02_conditions() {
    check("s02-conditions", "contacts-en");
}

#[test]
fn s03_results_vars() {
    check("s03-results-vars", "contacts-en");
}

#[test]
fn s04_server() {
    check("s04-server", "contacts-en");
}

#[test]
fn s05_transactions() {
    check("s05-transactions", "contacts-en");
}

#[test]
fn s06_halt_state() {
    check("s06-halt-state", "contacts-en");
}

#[test]
fn s07_transaction_errors() {
    check("s07-transaction-errors", "contacts-en");
}

#[test]
fn s08_queue() {
    check("s08-queue", "contacts-en");
}

#[test]
fn s09_step_ids() {
    check("s09-step-ids", "contacts-en");
}

#[test]
fn s10_fms_sample_triggers() {
    check("s10-fms-sample-triggers", "fms-sample-en");
}

#[test]
fn s11_queue_nested() {
    check("s11-queue-nested", "contacts-en");
}

#[test]
fn s12_transactions_nested() {
    check("s12-transactions-nested", "contacts-en");
}

#[test]
fn s13_runtime_notes() {
    check("s13-runtime-notes", "contacts-en");
}

#[test]
fn s14_triggers() {
    check("s14-triggers", "contacts-en");
}

/// The fixture against the Contacts template, when it is installed (the
/// shipped templates are not in the repository):
///   cargo test -p om-scripts --test oracle -- --ignored
#[test]
#[ignore]
fn the_fixture_matches_the_template() {
    const TEMPLATES: [&str; 2] = [
        "../../re/extracted/mac/FileMaker Pro 26/FileMaker Pro.app/Contents/Resources/DynamicTemplates",
        "/Applications/FileMaker Pro.app/Contents/Resources/DynamicTemplates",
    ];
    let Some(path) = TEMPLATES
        .iter()
        .map(|dir| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(dir)
                .join("Contacts/en.fmp12")
        })
        .find(|p| p.exists())
    else {
        eprintln!("the Contacts template is not installed");
        return;
    };
    let file = om_hbam::HbamFile::open(&path).expect("template");
    let tree = file.key_tree().expect("key tree");
    // Catalog keys are serial ids (file-metadata.md 1).
    let serial = |raw: u64| {
        let bytes = raw.to_be_bytes();
        let first = bytes.iter().position(|b| *b != 0).unwrap_or(7);
        om_hbam::serial::decode(&bytes[first..]).expect("serial key")
    };
    let catalog = om_hbam::Catalog::from_tree(&tree).expect("catalog");
    let table = catalog.table_by_name("Contacts").expect("Contacts table");
    assert_eq!(serial(table.id), contacts::CONTACTS_TABLE);
    let fields: Vec<(u64, String, bool)> = table
        .fields
        .iter()
        .map(|f| {
            let calc = f.kind == om_hbam::FieldKind::Calculation;
            (serial(f.id), f.name.clone(), calc)
        })
        .collect();
    let ours: Vec<(u64, String, bool)> = contacts::FIELDS
        .iter()
        .map(|&(id, name, _)| (id, name.to_owned(), id == contacts::INITIAL))
        .collect();
    assert_eq!(fields, ours);
    let occurrence = catalog
        .table_occurrences
        .iter()
        .find(|o| o.name == "Contacts")
        .expect("Contacts occurrence");
    assert_eq!(serial(occurrence.id), contacts::CONTACTS);
    for (id, name) in contacts::LAYOUTS {
        let layout = catalog.layout_by_name(name).expect("layout");
        assert_eq!(serial(layout.id), id, "{name}");
    }
    let scripts = om_scripts::ScriptCatalog::decode(&tree).expect("scripts");
    let sort = scripts
        .scripts
        .iter()
        .find(|s| s.name == "Sort by Name" && s.kind == ScriptKind::Script)
        .expect("Sort by Name");
    assert_eq!(serial(sort.id), contacts::SORT_BY_NAME);
    let steps: Vec<u16> = sort.steps.iter().map(|s| s.id()).collect();
    assert_eq!(steps, [39]);
}
