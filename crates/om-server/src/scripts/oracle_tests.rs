//! The script oracle's validation battery s18 (`tools/fms-script-harness`,
//! script-runtime.md 13.21) replayed on a server script session: every run
//! line of each `s18-<rule>-<variant>` result, with the same fields and the
//! same steps as its probe script, gives FileMaker Server's errors.
//!
//! The probe files are the Contacts starter template with four fields
//! given one rule (A First Name and C Title without repetitions, B Last
//! Name and D Job Title with three); here they are fields of the Tasks
//! fixture with the definitions the battery's header names, beside E
//! (Website) and F (Company, the values of the "Company" list). The values
//! a probe reads back are compared as text for text and number fields;
//! dates and times, which the generated files show in formats of their
//! own, only by whether there is one and whether it is `?`.

use std::path::{Path, PathBuf};

use om_calc::{Ctx, Value};
use om_hbam::slot::uint_segment;
use om_hbam::OwnedNode;
use om_scripts::runtime::{
    FieldRef, Fields, HostError, HostResult, Navigation, RecordMove, Records, TransactionOptions,
};

use super::fixture::{self, TempFile};
use super::tests::{session, shared};
use super::*;

/// The probe fields: letter, the Contacts field it is in the probe file
/// and its id here.
const FIELDS: [(char, u64, u64); 6] = [
    ('A', 1, 30),
    ('B', 2, 31),
    ('C', 4, 32),
    ('D', 6, 33),
    ('E', 7, 34),
    ('F', 5, 35),
];

/// The value lists of the Contacts template the rules name: 3 "Title"
/// (custom) and 5 "Company" (the values of field F).
const TITLE: &str = "Mr.\rMrs.\rMs.\r-\rProf.\r-";

fn results_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/fms-script-harness")
}

/// A field definition from a battery's `<field>=<type>,<validation
/// hex>,<repetitions>[,<options hex>[,<maximum characters>]]`.
struct Spec {
    contacts: u64,
    data_type: u8,
    validation: u32,
    repetitions: u16,
    options: u32,
    limit: u32,
}

fn specs(battery: &str) -> Vec<Spec> {
    let line = battery
        .lines()
        .find(|l| l.contains("s18_validation.sh, fields"))
        .expect("the battery names its fields");
    line.split("fields ")
        .nth(1)
        .unwrap()
        .split_whitespace()
        .map(|spec| {
            let (field, rest) = spec.split_once('=').unwrap();
            let parts: Vec<&str> = rest.split(',').collect();
            Spec {
                contacts: field.parse().unwrap(),
                data_type: parts[0].parse().unwrap(),
                validation: u32::from_str_radix(parts[1], 16).unwrap(),
                repetitions: parts[2].parse().unwrap(),
                options: parts
                    .get(3)
                    .map_or(0, |o| u32::from_str_radix(o, 16).unwrap()),
                limit: parts.get(4).map_or(0, |l| l.parse().unwrap()),
            }
        })
        .collect()
}

/// The 26-byte field definition (schema-path.md 5.5).
fn definition(data_type: u8, options: u32, validation: u32, limit: u32, reps: u16) -> Vec<u8> {
    let mut out = vec![0; 26];
    out[0] = 1;
    out[1] = data_type;
    out[4..8].copy_from_slice(&21u32.to_be_bytes());
    out[8..12].copy_from_slice(&options.to_be_bytes());
    out[12..16].copy_from_slice(&validation.to_be_bytes());
    out[20..24].copy_from_slice(&limit.to_be_bytes());
    out[24..26].copy_from_slice(&reps.to_be_bytes());
    out
}

/// The Tasks fixture with the probe file's fields, range and value lists.
fn probe_tree(rule: &str, specs: &[Spec]) -> OwnedNode {
    let mut root = fixture::tree(&[], true);
    let t = uint_segment(fixture::TABLE);
    let put_field = |root: &mut OwnedNode, id: u64, name: &str, definition: Vec<u8>| {
        let f = uint_segment(id);
        root.put(
            &[&t, &[3], &[5], &f, &[2]],
            OwnedNode::leaf(Vec::new(), definition),
        );
        root.put(
            &[&t, &[3], &[5], &f, &[0x10]],
            om_scripts::text_node(&[], name),
        );
    };
    for (letter, contacts, id) in FIELDS {
        let name = format!("Probe {letter}");
        match specs.iter().find(|s| s.contacts == contacts) {
            Some(s) => {
                // FileMaker indexes a field validated as an existing value
                // (the generated files' definitions).
                let index = if s.validation & 0x20 != 0 {
                    0x8000_0000
                } else {
                    0
                };
                let def = definition(
                    s.data_type,
                    s.options | index,
                    s.validation,
                    s.limit,
                    s.repetitions,
                );
                put_field(&mut root, id, &name, def);
            }
            // E and F are plain text; F feeds the "Company" list.
            None => put_field(&mut root, id, &name, definition(1, 0x8000_0000, 0, 0, 1)),
        }
    }
    for spec in specs {
        let id = FIELDS.iter().find(|f| f.1 == spec.contacts).unwrap().2;
        let f = uint_segment(id);
        if rule == "range" {
            root.put(
                &[&t, &[3], &[5], &f, &[0x21]],
                om_scripts::text_node(&[], "1"),
            );
            root.put(
                &[&t, &[3], &[5], &f, &[0x22]],
                om_scripts::text_node(&[], "10"),
            );
        }
        let list = match rule {
            "vlcustom" => Some(3),
            "vlfield" => Some(5),
            _ => None,
        };
        if let Some(list) = list {
            let reference = vec![2, 1, list, 0x90, 0, 0, 2, 1, 0, 0, 0, 0, 0xff];
            root.put(
                &[&t, &[3], &[5], &f, &[0x07]],
                OwnedNode::leaf(Vec::new(), reference),
            );
        }
    }
    // [21, 5, 3] Title, [21, 5, 5] Company (kind 1: Tasks::Probe F).
    root.put(
        &[&[0x21], &[5], &[3], &[0x10]],
        om_scripts::text_node(&[], "Title"),
    );
    root.put(
        &[&[0x21], &[5], &[3], &[3]],
        om_scripts::text_node(&[], TITLE),
    );
    root.put(
        &[&[0x21], &[5], &[5], &[0x10]],
        om_scripts::text_node(&[], "Company"),
    );
    let mut company = vec![
        1, 0, 0, 4, 3, 0xd0, 0, 1, 2, 1, 35, 0, 1, 0, 0, 0, 0, 0, 0, 0,
    ];
    company.extend(om_scsu::encode_counted(" "));
    root.put(
        &[&[0x21], &[5], &[5], &[2]],
        OwnedNode::leaf(Vec::new(), company),
    );
    root
}

fn code(result: HostResult) -> i32 {
    match result {
        Ok(()) => 0,
        Err(HostError::Error(ErrorCode(code))) => code,
        Err(other) => panic!("{other:?}"),
    }
}

/// One probe run (the battery's "probe" script), or "seed" (the probe in
/// Open Transaction [ Skip data entry validation ]): what it exits with.
fn probe(shared: &Arc<Shared>, specs: &[Spec], parameter: &str, seed: bool) -> String {
    let mut host = session(shared);
    if seed {
        let options = TransactionOptions {
            skip_validation: true,
            ..Default::default()
        };
        host.begin_transaction(options).unwrap();
    }
    let lines: Vec<&str> = parameter.split('\r').collect();
    let last = lines[0] == "L";
    if last {
        host.go_to_record(RecordMove::Last, false).unwrap();
    } else {
        host.new_record().unwrap();
    }
    let mut out = String::new();
    for change in lines[1..].chunks(3) {
        let letter = change[0].chars().next().unwrap();
        let repetition: u32 = change[1].parse().unwrap();
        let (_, contacts, id) = *FIELDS.iter().find(|f| f.0 == letter).unwrap();
        let field = FieldRef {
            occurrence: fixture::OCCURRENCE,
            field: id,
            repetition,
        };
        let set = code(host.set_field(field, Value::text(change[2])));
        let record = host.calc_context_for_test().record.unwrap();
        let value =
            CalcHost::field_value(&host, fixture::OCCURRENCE, record, id, repetition).unwrap();
        let text = value
            .to_text(&Ctx::new(host.locale(), host.today()))
            .into_string();
        let typed = specs
            .iter()
            .find(|s| s.contacts == contacts)
            .is_some_and(|s| (3..=5).contains(&s.data_type));
        let shown = match text.as_str() {
            "" | "?" => text,
            _ if typed => "<value>".into(),
            _ => text,
        };
        out.push_str(&format!("{set},{shown};"));
    }
    let commit = code(host.commit_records(Default::default()));
    if commit != 0 {
        if last {
            host.revert_record(false).unwrap();
        } else {
            host.delete_record(false).unwrap();
        }
    }
    out.push_str(&format!("/{commit}"));
    if seed {
        out.push_str(&format!("/{}", code(host.commit_transaction(false))));
    }
    host.close();
    out
}

/// What the oracle's result reads as here: dates and times as `<value>`.
fn expected(result: &str, specs: &[Spec], parameter: &str) -> String {
    let lines: Vec<&str> = parameter.split('\r').collect();
    // "set,value;...;/commit[/transaction]": the changes end with `;`.
    let (changes, rest) = match result.rfind(";/") {
        Some(at) => (&result[..=at], &result[at + 2..]),
        None => ("", result.trim_start_matches('/')),
    };
    let mut out = String::new();
    for (change, set) in lines[1..].chunks(3).zip(changes.split(';')) {
        let letter = change[0].chars().next().unwrap();
        let contacts = FIELDS.iter().find(|f| f.0 == letter).unwrap().1;
        let typed = specs
            .iter()
            .find(|s| s.contacts == contacts)
            .is_some_and(|s| (3..=5).contains(&s.data_type));
        let (error, value) = set.split_once(',').unwrap();
        let shown = match value {
            "" | "?" => value,
            _ if typed => "<value>",
            _ => value,
        };
        out.push_str(&format!("{error},{shown};"));
    }
    out.push('/');
    out.push_str(rest);
    out
}

/// Unescape a harness field (`\r`, `\n`, `\t`, `\\`).
fn unescape(text: &str) -> String {
    text.replace("\\r", "\r")
        .replace("\\n", "\n")
        .replace("\\t", "\t")
        .replace("\\\\", "\\")
}

#[test]
fn s18_validation_replays_as_filemaker_server_answered() {
    let dir = results_dir();
    let mut runs = 0;
    let mut differences = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(dir.join("batteries"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().into_string().unwrap())
        .filter(|n| n.starts_with("s18-") && n.ends_with(".txt") && n != "s18-find.txt")
        .collect();
    entries.sort();
    assert!(entries.len() >= 19, "{entries:?}");
    for name in entries {
        let name = name.trim_end_matches(".txt");
        let rule = name
            .trim_start_matches("s18-")
            .rsplit_once('-')
            .unwrap()
            .0
            .to_string();
        let battery =
            std::fs::read_to_string(dir.join("batteries").join(format!("{name}.txt"))).unwrap();
        let specs = specs(&battery);
        let results =
            std::fs::read_to_string(dir.join("results").join(format!("{name}.{name}.tsv")))
                .unwrap();
        let file = TempFile::from_tree(&format!("oracle-{name}"), &probe_tree(&rule, &specs));
        let shared = shared(&file);
        let records = |shared: &Shared| {
            let hosted = shared.hosting.by_name(fixture::FILE).unwrap().clone();
            let store = hosted.read_store();
            let t = uint_segment(fixture::TABLE);
            store.node(&[&t, &[5]]).map_or(0, |n| n.children.len())
        };
        let before = records(&shared);
        for line in results.lines().filter(|l| !l.starts_with(['>', '!', '#'])) {
            let fields: Vec<&str> = line.split('\t').collect();
            let (script, parameter, result) = (fields[0], unescape(fields[1]), fields[3]);
            runs += 1;
            let got = match script {
                "probe" | "seed" => probe(&shared, &specs, &parameter, script == "seed"),
                "count" => (records(&shared) - before).to_string(),
                other => panic!("{name}: unknown script {other}"),
            };
            let want = match script {
                "count" => result.to_string(),
                _ => expected(result, &specs, &parameter),
            };
            if got != want {
                differences.push(format!(
                    "{name} {script} {parameter:?}: {got} (FileMaker: {want})"
                ));
            }
        }
    }
    assert!(runs > 250, "{runs} runs");
    assert!(
        differences.is_empty(),
        "{} of {runs} runs differ:\n{}",
        differences.len(),
        differences.join("\n")
    );
}

/// The s19 batteries' formulas, with Company read as the Tasks fixture's
/// Name (the probe file's First Name is Code here, with three
/// repetitions).
const S19: [(&str, &str); 5] = [
    ("extbare", "Self = Name"),
    ("extindex1", "Self = Name[1]"),
    ("extgetrep", "Self = GetRepetition ( Name ; 1 )"),
    ("extextend", "Self = Extend ( Name )"),
    ("extindex2", "IsEmpty ( Name[2] )"),
];

/// A Set Field step of a probe: field, repetition, text.
type SetFieldStep = (u64, u32, String);

/// The probes of an s19 battery: each one's name and Set Field steps,
/// Company as Name and First Name as Code.
fn s19_probes(battery: &str) -> Vec<(String, Vec<SetFieldStep>)> {
    let mut out: Vec<(String, Vec<SetFieldStep>)> = Vec::new();
    for line in battery.lines() {
        if let Some(name) = line.strip_prefix("== ") {
            out.push((name.to_string(), Vec::new()));
        } else if let Some(rest) = line.strip_prefix("Set Field [ Contacts::") {
            let (target, value) = rest.trim_end_matches(" ]").split_once(" ; ").unwrap();
            let (name, repetition) = match target.split_once('[') {
                Some((name, rep)) => (name, rep.trim_end_matches(']').parse().unwrap()),
                None => (target, 1),
            };
            let field = match name {
                "Company" => fixture::NAME,
                "First Name" => fixture::CODE,
                other => panic!("{other}"),
            };
            let text = value.trim_matches('"').to_string();
            out.last_mut().unwrap().1.push((field, repetition, text));
        }
    }
    out
}

/// Battery s19 (script-runtime.md 13.22) replayed on a server session:
/// while repetition n is validated, a field that does not repeat reads its
/// value bare, as its repetition 1, through GetRepetition and Extend, and
/// its repetition 2 fails the validation, as FileMaker Server answered.
#[test]
fn s19_validation_replays_as_filemaker_server_answered() {
    let dir = results_dir();
    let mut runs = 0;
    let mut differences = Vec::new();
    for (rule, formula) in S19 {
        let name = format!("s19-validation-{rule}");
        let battery =
            std::fs::read_to_string(dir.join("batteries").join(format!("{name}.txt"))).unwrap();
        let results = std::fs::read_to_string(
            dir.join("results")
                .join(format!("{name}.contacts-{rule}.tsv")),
        )
        .unwrap();
        let mut root = fixture::tree(&[], true);
        fixture::set_field_repetitions(&mut root, fixture::TABLE, fixture::CODE, 3);
        fixture::add_formula(
            &mut root,
            &fixture::names(),
            (fixture::TABLE, fixture::OCCURRENCE),
            fixture::CODE,
            0x19,
            formula,
        );
        let file = TempFile::from_tree(&format!("oracle-{name}"), &root);
        let shared = shared(&file);
        let mut created = 0;
        let probes = s19_probes(&battery);
        for line in results.lines().filter(|l| !l.starts_with(['>', '!', '#'])) {
            let fields: Vec<&str> = line.split('\t').collect();
            let (script, want) = (fields[0], fields[3]);
            runs += 1;
            let got = if script == "count" {
                created.to_string()
            } else {
                let (_, steps) = probes.iter().find(|(n, _)| n == script).unwrap();
                let mut host = session(&shared);
                host.new_record().unwrap();
                let mut out = String::new();
                for (field, repetition, text) in steps {
                    let field = FieldRef {
                        occurrence: fixture::OCCURRENCE,
                        field: *field,
                        repetition: *repetition,
                    };
                    out.push_str(&code(host.set_field(field, Value::text(text))).to_string());
                }
                let commit = code(host.commit_records(Default::default()));
                if commit == 0 {
                    created += 1;
                } else {
                    host.delete_record(false).unwrap();
                }
                host.close();
                format!("{out}/{commit}")
            };
            if got != want {
                differences.push(format!("{name} {script}: {got} (FileMaker: {want})"));
            }
        }
    }
    assert_eq!(runs, 35);
    assert!(
        differences.is_empty(),
        "{} of {runs} runs differ:\n{}",
        differences.len(),
        differences.join("\n")
    );
}
