//! The design reference graph (`om_data::design_refs`) of every shipped
//! template: it builds; every reference resolves to an object the graph
//! lists, or is reported broken, external or unresolved; and the uses two
//! decoders read on their own paths appear in where-used: each layout
//! field object's field, each button's and each Perform Script step's
//! script. Prints the
//! counts of files, objects and references, and of the places that could
//! not be read, by reason and by kind: the analysis's real limit.
//!
//! Ignored by default: the templates exist only on the analysis host
//! (`re/extracted`, git-ignored). Run with
//!   cargo test --release -p om-data --test design_refs_corpus -- --ignored --nocapture

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use om_data::design_refs::{self, Kind, ObjectId, Resolution, Target};
use om_hbam::HbamFile;

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

#[derive(Default, Debug)]
struct Counts {
    files: usize,
    objects: BTreeMap<&'static str, usize>,
    references: usize,
    references_by_place: BTreeMap<String, usize>,
    resolved: usize,
    broken: usize,
    external: usize,
    unresolved: usize,
    read: BTreeMap<&'static str, usize>,
    unread_by_reason: BTreeMap<&'static str, usize>,
    unread_by_holder: BTreeMap<&'static str, usize>,
    unread_details: BTreeMap<String, usize>,
    by_name: BTreeMap<&'static str, usize>,
    layout_fields_checked: usize,
    button_performs_checked: usize,
    performs_checked: usize,
}

#[test]
#[ignore = "requires the shipped templates under re/extracted (and the FileMaker Server samples when present)"]
fn every_template_builds_and_every_reference_resolves() {
    let mut paths = Vec::new();
    if let Some(dir) = TEMPLATES.iter().map(Path::new).find(|d| d.is_dir()) {
        files(dir, &mut paths);
    }
    files(Path::new(FMS_SAMPLES), &mut paths);
    assert!(!paths.is_empty(), "no template files found");
    paths.sort();
    let mut counts = Counts::default();
    for path in &paths {
        check(path, &mut counts);
    }
    println!("{counts:#?}");
}

fn check(path: &Path, counts: &mut Counts) {
    let at = path.display().to_string();
    let file = HbamFile::open(path).unwrap_or_else(|e| panic!("{at}: {e}"));
    let tree = file.key_tree().unwrap_or_else(|e| panic!("{at}: {e}"));
    let graph = design_refs::build(&tree, Vec::new());
    counts.files += 1;
    for object in graph.objects() {
        *counts.objects.entry(object.id.kind().name()).or_default() += 1;
    }
    for u in graph.uses() {
        counts.references += 1;
        let part: String = u
            .place
            .part
            .chars()
            .map(|c| if c.is_ascii_digit() { '#' } else { c })
            .collect();
        let place = match &u.place.step {
            Some(step) => format!("script step {} {part}", step.name),
            None => format!("{} {part}", u.holder.kind().name()),
        };
        *counts.references_by_place.entry(place).or_default() += 1;
        match &u.resolution {
            Resolution::Resolved(id) => {
                counts.resolved += 1;
                assert!(
                    graph.object(*id).is_some(),
                    "{at}: {u:?} resolves to no listed object"
                );
            }
            Resolution::Broken { missing } => {
                counts.broken += 1;
                assert!(
                    graph.object(*missing).is_none(),
                    "{at}: {u:?} is missing a listed object"
                );
            }
            Resolution::External { data_source } => {
                counts.external += 1;
                assert!(
                    graph.object(ObjectId::DataSource(*data_source)).is_some(),
                    "{at}: {u:?}"
                );
            }
            Resolution::Unresolved { .. } => counts.unresolved += 1,
        }
        assert!(
            graph.object(u.holder).is_some() || u.holder == ObjectId::File,
            "{at}: the holder of {u:?} is not listed"
        );
    }
    let coverage = graph.coverage();
    for (what, n) in &coverage.read {
        *counts.read.entry(what).or_default() += n;
    }
    for u in &coverage.unread {
        *counts.unread_by_reason.entry(u.reason.code()).or_default() += 1;
        *counts
            .unread_by_holder
            .entry(u.holder.kind().name())
            .or_default() += 1;
        let detail = match u.place.step.as_ref() {
            Some(step) => format!(
                "{} step {} ({}): {}",
                u.holder.kind().name(),
                step.id,
                step.name,
                u.detail
            ),
            None => format!("{} {}: {}", u.holder.kind().name(), u.place.part, u.detail),
        };
        // Group details by their text without numbers.
        let detail: String = detail
            .chars()
            .map(|c| if c.is_ascii_digit() { '#' } else { c })
            .collect();
        *counts.unread_details.entry(detail).or_default() += 1;
    }
    for b in &coverage.by_name {
        *counts.by_name.entry(b.how.code()).or_default() += 1;
    }
    // A layout field object's field, read by `om_layout` directly.
    for layout in file.catalog().unwrap().layouts {
        let Ok(Some(decoded)) = om_layout::Layout::decode_from_tree(&tree, layout.id) else {
            continue;
        };
        for object in &decoded.objects.objects {
            let Some(spec) = object.field_spec() else {
                continue;
            };
            let (Some(occurrence), Some(field)) =
                (om_layout::occurrence_id(spec.table_key), spec.field_id())
            else {
                continue;
            };
            let holder = ObjectId::LayoutObject {
                layout: layout.id,
                object: object.id,
            };
            let found = graph
                .uses()
                .iter()
                .any(|u| u.holder == holder && u.target == Target::Field { occurrence, field });
            assert!(
                found,
                "{at}: layout {} object {} field",
                layout.id, object.id
            );
            if let Some(table) = graph.occurrence_table(occurrence) {
                let id = ObjectId::Field { table, field };
                if graph.object(id).is_some() {
                    assert!(
                        graph.uses_of(id).iter().any(|u| u.holder == holder),
                        "{at}: where-used of {id:?} misses layout object {holder:?}"
                    );
                }
            }
            counts.layout_fields_checked += 1;
        }
        // A button's Perform Script, its key read from the stored step.
        for object in &decoded.objects.objects {
            let Some(step) = object.script_step().filter(|s| s.id == 1) else {
                continue;
            };
            let Some(script) =
                om_data::scripts::StoredKey::from_bytes(&step.reserved).and_then(|k| k.serial_id())
            else {
                continue;
            };
            let holder = ObjectId::LayoutObject {
                layout: layout.id,
                object: object.id,
            };
            let target = ObjectId::Script(script);
            if graph.object(target).is_some() {
                assert!(
                    graph.uses_of(target).iter().any(|u| u.holder == holder),
                    "{at}: where-used of {target:?} misses button {holder:?}"
                );
                counts.button_performs_checked += 1;
            }
        }
    }
    // Each Perform Script from the list, read with `ScriptStep::script_id`.
    let scripts = om_data::ScriptCatalog::decode(&tree).unwrap_or_else(|e| panic!("{at}: {e}"));
    for script in &scripts.scripts {
        for (i, step) in script.steps.iter().enumerate() {
            let local = !step.has_flag(om_data::flag::FILE_REFERENCE);
            if step.id() != 1 || step.list_value(1) == Some(1) || !local {
                continue;
            }
            let Some(callee) = step.script_id() else {
                continue;
            };
            let holder = ObjectId::Script(script.id);
            let target = ObjectId::Script(callee);
            let found = graph.uses().iter().any(|u| {
                u.holder == holder
                    && u.place.step.as_ref().is_some_and(|s| s.index == i + 1)
                    && u.target == Target::Object(target)
            });
            assert!(
                found,
                "{at}: script {} step {} performs {callee}",
                script.id,
                i + 1
            );
            if graph.object(target).is_some() && callee != script.id {
                assert!(graph.uses_of(target).iter().any(|u| u.holder == holder));
            }
            counts.performs_checked += 1;
        }
    }
    // The kinds every unused check could be uncertain for.
    for kind in Kind::REFERABLE {
        let _ = graph.unused(kind);
    }
}
