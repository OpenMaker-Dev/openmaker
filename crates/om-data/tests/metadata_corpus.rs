//! Every file-metadata structure of the shipped templates decodes
//! (file-metadata.md 9): the locale record (and re-encodes byte for byte),
//! the document preferences, relationships, summary definitions, value
//! lists, layout, object and table-view trigger records (re-encoded byte
//! for byte, parameters included), script ids as serial keys and the
//! catalogs' display orders, and the calculation schema built from them.
//!
//! Ignored by default: the samples exist only on the analysis host. Run with
//!   cargo test --release -p om-data --test metadata_corpus -- --ignored --nocapture
//! `OM_METADATA_CORPUS` adds files or directories (`:`-separated), such as
//! the MBS examples in `re/work/calc-encoding/corpus-mbs`.

use std::path::{Path, PathBuf};

use om_data::calc::Locale;
use om_data::meta::{self, FileOptions, Relationship, SummaryDef, SummaryKind, ValueList};
use om_hbam::{HbamFile, KeyTree, Node};

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
    relationships: usize,
    sorted_sides: usize,
    summaries: usize,
    value_lists: usize,
    trigger_records: usize,
    trigger_parameters: usize,
    table_view_triggers: usize,
    file_triggers: usize,
    scripts_from_128: usize,
}

fn members<'n>(tree: &'n KeyTree<'_>, path: &[u64]) -> Vec<&'n Node<'n>> {
    tree.at(path)
        .into_iter()
        .flat_map(|n| n.children.iter())
        .filter(|n| n.id().is_some_and(|id| id != 0))
        .collect()
}

/// Check one trigger cache; returns its records and parameters.
fn triggers(cache: &Node<'_>, at: &str) -> (usize, usize) {
    let (mut records, mut parameters) = (0, 0);
    for entry in cache.get(5).map(|n| n.children.as_slice()).unwrap_or(&[]) {
        let Some(bytes) = entry.get(1).and_then(Node::assembled) else {
            panic!("{at}: event {} has no record", entry.segment);
        };
        let trigger = om_layout::triggers::decode_record(&bytes)
            .unwrap_or_else(|| panic!("{at}: record {bytes:02x?} does not decode"));
        assert_eq!(
            om_layout::triggers::encode_record(&trigger),
            *bytes,
            "{at}: record re-encodes"
        );
        records += 1;
        if let Some(parameter) = entry.get(0x31) {
            assert!(
                parameter
                    .get(6)
                    .and_then(Node::assembled)
                    .is_some_and(|p| om_data::formula::polish::decode(&p).is_ok()),
                "{at}: parameter decodes"
            );
            parameters += 1;
        }
    }
    assert_eq!(
        records,
        om_layout::triggers::decode(cache).len(),
        "{at}: every record is kept"
    );
    (records, parameters)
}

fn check(path: &Path, counts: &mut Counts) {
    let at = path.display().to_string();
    let file = HbamFile::open(path).unwrap_or_else(|e| panic!("{at}: {e}"));
    let tree = file.key_tree().unwrap_or_else(|e| panic!("{at}: {e}"));
    let catalog = om_hbam::Catalog::from_tree(&tree).unwrap_or_else(|e| panic!("{at}: {e}"));
    counts.files += 1;

    let record = tree
        .at(&meta::LOCALE)
        .and_then(Node::assembled)
        .unwrap_or_else(|| panic!("{at}: no locale record"));
    let locale =
        Locale::from_record(&record).unwrap_or_else(|| panic!("{at}: locale does not decode"));
    assert_eq!(locale.to_record(), *record, "{at}: locale re-encodes");

    if let Some(bytes) = tree
        .at(&meta::DOCUMENT_PREFERENCES)
        .and_then(Node::assembled)
    {
        let options = FileOptions::decode(&bytes).unwrap_or_else(|| panic!("{at}: preferences"));
        counts.file_triggers += [201, 202, 204, 205, 206, 209]
            .iter()
            .filter(|e| options.trigger(**e).is_some())
            .count();
    }

    for entry in members(&tree, &meta::RELATIONSHIPS) {
        let r = Relationship::from_node(entry)
            .unwrap_or_else(|| panic!("{at}: relationship {} does not decode", entry.segment));
        counts.relationships += 1;
        counts.sorted_sides +=
            usize::from(r.order_of(r.left).is_some()) + usize::from(r.order_of(r.right).is_some());
    }

    for table in &catalog.tables {
        for field in table
            .fields
            .iter()
            .filter(|f| f.kind == om_hbam::FieldKind::Summary)
        {
            let bytes = tree
                .at(&[table.id, 3, 5, field.id, 9])
                .and_then(Node::assembled)
                .unwrap_or_default();
            let def = SummaryDef::decode(&bytes);
            assert!(
                !matches!(def.kind, SummaryKind::Other(_)),
                "{at}: summary {} kind {:?}",
                field.name,
                def.kind
            );
            counts.summaries += 1;
        }
    }

    for entry in members(&tree, &meta::VALUE_LISTS) {
        ValueList::from_node(entry)
            .unwrap_or_else(|| panic!("{at}: value list {} does not decode", entry.segment));
        counts.value_lists += 1;
    }

    for layout in members(&tree, &[4, 5]) {
        let id = layout.segment;
        if let Some(cache) = layout.get(0x30) {
            let (r, p) = triggers(cache, &format!("{at} layout {id}"));
            counts.trigger_records += r;
            counts.trigger_parameters += p;
        }
        for object in layout.get(5).map(|n| n.children.as_slice()).unwrap_or(&[]) {
            if let Some(cache) = object.get(0x30) {
                let (r, p) = triggers(
                    cache,
                    &format!("{at} layout {id} object {}", object.segment),
                );
                counts.trigger_records += r;
                counts.trigger_parameters += p;
            }
        }
        for object in layout
            .at(&[0x0d, 5])
            .map(|n| n.children.as_slice())
            .unwrap_or(&[])
        {
            if let Some(cache) = object.get(0x30) {
                let (r, p) = triggers(
                    cache,
                    &format!("{at} layout {id} table view {}", object.segment),
                );
                counts.table_view_triggers += r;
                counts.trigger_parameters += p;
            }
        }
    }

    for script in members(&tree, &[meta::SCRIPTS, 5]) {
        let id = om_hbam::serial::decode(script.segment.0)
            .unwrap_or_else(|| panic!("{at}: script key {} is not serial", script.segment));
        counts.scripts_from_128 += usize::from(id >= 128);
    }
    let scripts = meta::scripts(&tree);
    let catalog_scripts = om_data::ScriptCatalog::decode(&tree)
        .unwrap_or_else(|e| panic!("{at}: {}", e.0))
        .scripts;
    assert_eq!(scripts.len(), catalog_scripts.len(), "{at}: script entries");
    for s in &catalog_scripts {
        assert!(
            scripts.iter().any(|e| e.id == s.id),
            "{at}: script {}",
            s.id
        );
    }

    for catalog in [&[3u64, 16][..], &[3, 17], &[4], &[meta::SCRIPTS], &[0x21]] {
        meta::display_order(&tree, catalog);
    }
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned());
    let schema = om_data::rules::schema::calc_schema(&tree, &catalog, &name.unwrap_or_default());
    assert_eq!(schema.locale, locale, "{at}: the schema's locale");
    assert_eq!(
        schema.relationships().len(),
        members(&tree, &meta::RELATIONSHIPS).len()
    );
}

#[test]
#[ignore = "requires the shipped templates (and optionally the MBS corpus)"]
fn every_metadata_structure_decodes() {
    let mut paths = Vec::new();
    if let Some(dir) = TEMPLATES.iter().map(Path::new).find(|d| d.is_dir()) {
        files(dir, &mut paths);
    }
    files(Path::new(FMS_SAMPLES), &mut paths);
    assert!(!paths.is_empty(), "no sample files found");
    for entry in std::env::var("OM_METADATA_CORPUS")
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
    let mut counts = Counts::default();
    for path in &paths {
        check(path, &mut counts);
    }
    println!("{counts:#?}");
}

/// Every checksum of the security catalog `[23]` FileMaker checks at open
/// recomputes to what the file stores (local-files.md 2.4,
/// session-identity.md 4): each account's, privilege set's, extended
/// privilege's and file access entry's `04` and the file access catalog's
/// `09`.
#[test]
#[ignore = "requires the shipped templates (and optionally the MBS corpus)"]
fn every_security_checksum_recomputes() {
    use om_data::security_checksum::{self as checksum, FileStamp};
    let mut paths = Vec::new();
    if let Some(dir) = TEMPLATES.iter().map(Path::new).find(|d| d.is_dir()) {
        files(dir, &mut paths);
    }
    assert!(!paths.is_empty(), "no sample files found");
    let mut checked = 0;
    for path in &paths {
        let ctx = path.display();
        let file = HbamFile::open(path).unwrap();
        let status = |key: u8| {
            file.subtree(&[&[2], &[key]])
                .unwrap()
                .and_then(|n| n.assembled().map(|v| v.to_vec()))
        };
        let security = file.subtree(&[&[0x17]]).unwrap().unwrap().to_owned_node();
        let uuid = security
            .at(&[&[4], &[1], &[0x10]])
            .and_then(|n| n.value.as_deref())
            .and_then(|v| v.try_into().ok());
        let stamp = FileStamp {
            internal_creation: status(0xfe).unwrap().try_into().unwrap(),
            file_access_uuid: uuid,
            version: u16::from_be_bytes(status(5).unwrap().try_into().unwrap()),
        };
        let stored = |member: &om_hbam::OwnedNode, key: u8| {
            member.child(&[key]).and_then(|n| n.value.clone()).unwrap()
        };
        let members = |catalog: u8| {
            security
                .at(&[&[catalog], &[5]])
                .map(|c| c.children.clone())
                .unwrap_or_default()
        };
        for member in members(1) {
            let sum = checksum::account(&stamp, &member.segment, &member);
            assert_eq!(sum.unwrap().to_vec(), stored(&member, 4), "{ctx} account");
            checked += 1;
        }
        for member in members(2) {
            let sum = checksum::privilege_set(&stamp, &member);
            assert_eq!(sum.unwrap().to_vec(), stored(&member, 4), "{ctx} set");
            checked += 1;
        }
        for member in members(3) {
            let sum = checksum::extended_privilege(&member);
            assert_eq!(sum.unwrap().to_vec(), stored(&member, 4), "{ctx} ext");
            checked += 1;
        }
        for member in members(4) {
            let sum = checksum::file_access_entry(&stamp, &member.segment, &member);
            assert_eq!(
                sum.unwrap().to_vec(),
                stored(&member, 4),
                "{ctx} file access"
            );
            checked += 1;
        }
        let header = security.at(&[&[4], &[1]]).unwrap();
        let requires = u32::from_be_bytes(stored(header, 0xf0).try_into().unwrap());
        let sum = checksum::file_access_catalog(&stamp, requires);
        assert_eq!(sum.unwrap().to_vec(), stored(header, 9), "{ctx} catalog");
        checked += 1;
    }
    println!("{} files, {checked} checksums", paths.len());
}
