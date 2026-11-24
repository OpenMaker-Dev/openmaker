//! Validate om-hbam against the shipped FileMaker files.
//!
//! Ignored by default: the templates and the FileMaker Server sample are
//! git-ignored extractions that exist only on the analysis host. Run with:
//!   cargo test -p om-hbam --test templates -- --ignored
mod reference;

use std::path::{Path, PathBuf};

use om_hbam::catalog::keys;
use om_hbam::slot::{encode_chunk, minimal_code};
use om_hbam::{ChunkKind, DataType, FieldKind, HbamFile, KeyTree};

const WIN_TEMPLATES: &str =
    "../../re/extracted/win/msi/PFiles64/FileMaker/FileMaker Pro/DynamicTemplates";
const MAC_TEMPLATES: &str =
    "../../re/extracted/mac/FileMaker Pro 26/FileMaker Pro.app/Contents/Resources/DynamicTemplates";
const FMS_SAMPLES: &str =
    "../../re/extracted/fms/deb/opt/FileMaker/FileMaker Server/Data/Databases/.Sample";

fn template(name: &str, lang: &str) -> HbamFile {
    HbamFile::open(
        Path::new(WIN_TEMPLATES)
            .join(name)
            .join(format!("{lang}.fmp12")),
    )
    .expect("template present")
}

fn sample(lang: &str) -> HbamFile {
    HbamFile::open(Path::new(FMS_SAMPLES).join(format!("{lang}_FMServer_Sample")))
        .expect("sample present")
}

fn all_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("directory present") {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "fmp12")
                || path.to_string_lossy().ends_with("FMServer_Sample")
            {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    for dir in [WIN_TEMPLATES, MAC_TEMPLATES, FMS_SAMPLES] {
        walk(Path::new(dir), &mut out);
    }
    out.sort();
    out
}

/// Every page of every shipped file decodes with no desync, and re-encoding
/// each header and chunk reproduces the page byte for byte.
#[test]
#[ignore = "requires the git-ignored extracted templates"]
fn every_file_parses_and_round_trips() {
    let files = all_files();
    assert!(files.len() >= 132 + 2, "found {} files", files.len());
    let mut chunks = 0usize;
    for path in &files {
        let ctx = path.display();
        let file = HbamFile::open(path).unwrap();
        let sb = file.superblock();
        assert_eq!(file.page_size(), 4096, "{ctx}");
        assert!(sb.signature_matches(), "{ctx}");
        assert_eq!(sb.version_strings(), ["HBAM2125JAN11", "Pro 12.0"], "{ctx}");
        assert_eq!(file.leaf_head().unwrap(), 2, "{ctx}");

        let leaves = file.leaf_pages().collect::<Result<Vec<_>, _>>().unwrap();
        // Compacted files: superblock + root + leaves account for every page.
        assert_eq!(leaves.len() as u32 + 2, file.page_count(), "{ctx}");
        // Page 1 is the root. Its NextPage is often nonzero (a leaf page
        // number; meaning unknown), so it is not checked.
        let root = file.page(1).unwrap();
        assert!(root.header.level > 0 && root.header.prev == 0, "{ctx}");
        let tail = leaves.last().unwrap();
        // The chain is one bracketed list: it opens with 0x80 and the tail's
        // last chunk closes the root.
        assert_eq!(leaves[0].used.first(), Some(&0x80), "{ctx}");
        let mut tail_slots = tail.slots();
        tail_slots.by_ref().for_each(|s| drop(s.unwrap()));
        assert!(
            tail_slots.root_closed() && tail.used.last() == Some(&0x40),
            "{ctx}"
        );

        // List brackets: 0x80 follows exactly the pushes that create a node;
        // a page continuing an open node re-pushes its path without 0x80.
        let mut seen = std::collections::HashSet::new();
        for page in &leaves {
            let mut stack: Vec<&[u8]> = Vec::new();
            let mut pending_new: Option<bool> = None;
            for c in page.chunks() {
                let c = c.unwrap();
                if let Some(created) = pending_new.take() {
                    assert_eq!(
                        created,
                        c.kind == ChunkKind::Begin,
                        "{ctx} page {}",
                        page.number
                    );
                }
                match c.kind {
                    ChunkKind::Pop => drop(stack.pop()),
                    ChunkKind::Push => {
                        stack.push(c.key);
                        pending_new = Some(seen.insert(stack.clone()));
                    }
                    _ => {}
                }
            }
        }

        let mut rebuilt = file.superblock().raw.to_vec();
        for n in 1..file.page_count() {
            let page = file.page(n).unwrap();
            assert_eq!(page.header.log_seq_num, 0, "{ctx} page {n}");
            let mut bytes = page.header.to_bytes().to_vec();
            if page.header.level != 0 {
                // Index pages use codes outside the leaf grammar (file-format.md
                // 8, open question 2); they are preserved, not decoded.
                assert_eq!(
                    &page.raw[bytes.len()..],
                    &[page.used, page.slack].concat()[..],
                    "{ctx} page {n}"
                );
                rebuilt.extend_from_slice(page.raw);
                continue;
            }
            for chunk in page.chunks() {
                let c = chunk.unwrap_or_else(|e| panic!("{ctx} page {n}: {e}"));
                chunks += 1;
                if matches!(c.kind, ChunkKind::Push | ChunkKind::Leaf) {
                    let value = c.has_value().then_some(c.value.len());
                    let push = c.kind == ChunkKind::Push;
                    assert_eq!(
                        minimal_code(push, c.key.len(), value),
                        Some(c.code),
                        "{ctx} page {n}"
                    );
                    assert_eq!(
                        encode_chunk(c.code, c.key, c.value).as_deref(),
                        Some(c.raw),
                        "{ctx} page {n}"
                    );
                }
                bytes.extend_from_slice(c.raw);
            }
            assert_eq!(
                bytes.len(),
                page.raw.len() - page.slack.len(),
                "{ctx} page {n}"
            );
            bytes.extend_from_slice(page.slack);
            assert_eq!(bytes, page.raw, "{ctx} page {n}");
            rebuilt.extend_from_slice(&bytes);
        }
        assert_eq!(rebuilt, file.as_bytes(), "{ctx}");

        // Key order holds across the whole leaf chain, and every catalog decodes.
        let tree = file.key_tree().unwrap_or_else(|e| panic!("{ctx}: {e}"));
        let catalog = om_hbam::Catalog::from_tree(&tree).unwrap_or_else(|e| panic!("{ctx}: {e}"));
        assert!(
            !catalog.tables.is_empty() && !catalog.layouts.is_empty(),
            "{ctx}"
        );
        for table in &catalog.tables {
            assert!(!table.name.is_empty() && !table.fields.is_empty(), "{ctx}");
            assert!(
                table
                    .fields
                    .iter()
                    .all(|f| !f.name.is_empty() && f.options.len() == 26),
                "{ctx}"
            );
        }
        for layout in &catalog.layouts {
            assert!(layout.layout_def.is_some(), "{ctx} layout {}", layout.id);
        }
    }
    println!("{} files, {chunks} chunks", files.len());
}

/// Windows and Mac copies are byte-identical (file-format.md "Primary samples").
#[test]
#[ignore = "requires the git-ignored extracted templates"]
fn mac_and_windows_copies_match() {
    let win = template("Contacts", "en");
    let mac = HbamFile::open(Path::new(MAC_TEMPLATES).join("Contacts/en.fmp12")).unwrap();
    assert_eq!(win.as_bytes(), mac.as_bytes());
}

#[test]
#[ignore = "requires the git-ignored extracted templates"]
fn contacts_schema() {
    let file = template("Contacts", "en");
    assert_eq!(file.page_count(), 140);
    let catalog = file.catalog().unwrap();

    let names: Vec<_> = catalog
        .tables
        .iter()
        .map(|t| (t.id, t.name.as_str()))
        .collect();
    assert_eq!(
        names,
        [
            (32770, "Contacts"),
            (32771, "Addresses"),
            (32772, "Phone Numbers"),
            (32773, "Email Addresses")
        ]
    );
    let contacts = catalog.table_by_name("Contacts").unwrap();
    let first = contacts.field(1).unwrap();
    assert_eq!(
        (first.name.as_str(), first.kind, first.data_type),
        ("First Name", FieldKind::Normal, DataType::Text)
    );
    assert_eq!(contacts.field(2).unwrap().name, "Last Name");
    assert_eq!(
        contacts.field_by_name("Photo").unwrap().data_type,
        DataType::Container
    );
    assert_eq!(contacts.field_by_name("Company").unwrap().id, 5);

    let names: Vec<_> = catalog.layouts.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names[..3], ["Contacts", "Contact List", "Contact Details"]);
    // "Contacts" is a blank layout: its object store is an empty value.
    assert_eq!(catalog.layouts[0].object_store.as_deref(), Some(&[][..]));
    for name in ["Contact List", "Contact Details"] {
        let layout = catalog.layout_by_name(name).unwrap();
        assert!(
            layout.object_store.as_ref().is_some_and(|s| !s.is_empty()),
            "{name}"
        );
        assert!(!layout.parts.is_empty(), "{name}");
    }
    assert!(catalog
        .table_occurrences
        .iter()
        .any(|o| o.name == "Contacts"));
    // Templates ship with no records.
    assert!(file.records(32770).unwrap().is_empty());
}

/// Two languages of one template share structure (ids, kinds, types) and
/// differ only in localized names; Japanese names are chunked SCSU text.
#[test]
#[ignore = "requires the git-ignored extracted templates"]
fn contacts_languages_share_structure() {
    let shape = |c: &om_hbam::Catalog| {
        let tables: Vec<_> = c
            .tables
            .iter()
            .map(|t| {
                (
                    t.id,
                    t.fields
                        .iter()
                        .map(|f| (f.id, f.kind, f.data_type))
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        let layouts: Vec<_> = c.layouts.iter().map(|l| l.id).collect();
        (tables, layouts)
    };
    let en = template("Contacts", "en").catalog().unwrap();
    for lang in ["de", "ja"] {
        let other = template("Contacts", lang).catalog().unwrap();
        assert_eq!(shape(&en), shape(&other), "{lang}");
        assert_ne!(
            en.tables[0].fields[0].name, other.tables[0].fields[0].name,
            "{lang}"
        );
    }
    let de = template("Contacts", "de").catalog().unwrap();
    assert_eq!(de.tables[0].field(1).unwrap().name, "Vorname");
    let ja = template("Contacts", "ja").catalog().unwrap();
    assert!(
        ja.tables[0].name.chars().all(|c| !c.is_ascii()),
        "{}",
        ja.tables[0].name
    );
}

/// A second template family validates the generic layout/table path.
#[test]
#[ignore = "requires the git-ignored extracted templates"]
fn other_templates() {
    let tasks = template("Tasks", "en").catalog().unwrap();
    assert!(tasks.table_by_name("Tasks").is_some());
    assert!(tasks.layouts.len() > 3);
    let inventory = template("Inventory", "en").catalog().unwrap();
    let product_fields: Vec<_> = inventory
        .tables
        .iter()
        .flat_map(|t| &t.fields)
        .map(|f| f.name.as_str())
        .collect();
    assert!(
        product_fields.contains(&"Reorder Level"),
        "{product_fields:?}"
    );
}

/// The FileMaker Server sample carries records: `<table>/5/<record>/<field>`.
#[test]
#[ignore = "requires the git-ignored FileMaker Server extraction"]
fn sample_records() {
    let file = sample("en");
    let catalog = file.catalog().unwrap();
    let tasks = catalog.table_by_name("Tasks").unwrap();
    assert_eq!(tasks.id, 32769);
    assert_eq!(tasks.field(2).unwrap().name, "Task");
    assert_eq!(tasks.field(3).unwrap().data_type, DataType::Date);
    assert!(catalog.layout_by_name("Task Details").is_some());

    let records = file.records(tasks.id).unwrap();
    assert_eq!(records.len(), 12);
    let first = &records[0];
    assert_eq!(first.id, 27);
    assert_eq!(
        file.cell(tasks.id, first.id, 2)
            .unwrap()
            .unwrap()
            .text()
            .unwrap()
            .unwrap(),
        "Site Map Sketch"
    );
    assert_eq!(file.cell(tasks.id, first.id, 0xff).unwrap(), None);
    let row = first.row();
    assert_eq!(row[&2], "Site Map Sketch");
    assert_eq!(row[&3], "2/17/2014");
    assert_eq!(row[&5], "Business");
    assert_eq!(row[&7], "In Progress");
    let values = first.values_for(tasks);
    assert_eq!(
        values[tasks.fields.iter().position(|f| f.id == 2).unwrap()].as_deref(),
        Some("Site Map Sketch")
    );

    // The object store is chunked (/0 = u32 length, /1.. = 1000-byte pieces).
    let layout = catalog.layout_by_name("Task Details").unwrap();
    assert_eq!(layout.object_store.as_ref().unwrap().len(), 0x21da);

    // Japanese values are chunked SCSU text.
    let ja = sample("ja");
    let ja_tasks = ja.records(32769).unwrap();
    let text = ja_tasks[0].text(2).unwrap().unwrap();
    assert!(!text.is_ascii() && !text.is_empty(), "{text}");
    assert_eq!(ja_tasks[0].text(3).unwrap().unwrap(), "2014/02/17");
}

/// The wire path: a packed slot buffer decodes through the same reader. A
/// page body is such a buffer; decoding it standalone with `SlotReader`
/// must match the file reader's view of that page.
#[test]
#[ignore = "requires the git-ignored FileMaker Server extraction"]
fn packed_buffer_matches_file_reader() {
    let file = sample("en");
    let mut from_pages = Vec::new();
    for page in file.leaf_pages() {
        let page = page.unwrap();
        let tree = KeyTree::from_slots(om_hbam::SlotReader::new(page.used)).unwrap();
        from_pages.extend(
            om_hbam::record::records(&tree, 32769)
                .into_iter()
                .map(|r| r.id),
        );
    }
    from_pages.dedup();
    let whole: Vec<_> = file.records(32769).unwrap().iter().map(|r| r.id).collect();
    assert_eq!(from_pages, whole);
    assert!(file
        .key_tree()
        .unwrap()
        .at(&[32769, keys::TABLE_RECORDS])
        .is_some());
}

/// `mInternalCreation` (`[2, 254]`, the Login nonce input) is a 4-byte value in
/// every shipped file. Contacts/en carries the value the hosted `test project`
/// inherited from it, confirmed by the shipped engine's own read.
#[test]
#[ignore = "requires git-ignored extracted templates"]
fn internal_creation_in_every_file() {
    assert_eq!(
        template("Contacts", "en").internal_creation().unwrap(),
        Some(0xdadc_ffd3)
    );
    let files = all_files();
    assert!(!files.is_empty());
    for path in files {
        let file = HbamFile::open(&path).unwrap();
        assert!(
            file.internal_creation().unwrap().is_some(),
            "{} has no mInternalCreation",
            path.display()
        );
    }
}

/// The local copy of the hosted test file (`re/work/login`, git-ignored).
#[test]
#[ignore = "requires a local copy of the hosted test project"]
fn internal_creation_of_test_project_copy() {
    let file = HbamFile::open("../../re/work/login/testproject.fmp12").unwrap();
    assert_eq!(file.internal_creation().unwrap(), Some(0xdadc_ffd3));
}

/// Every top-level subtree of every shipped file survives the wire form
/// (`Node::to_wire`, what a server sends for a Download): it decodes back to an
/// identical tree, page-boundary re-pushes and all.
#[test]
#[ignore = "requires the git-ignored extracted templates"]
fn every_subtree_round_trips_through_the_wire_form() {
    use om_hbam::{KeySegment, SlotReader};
    for path in all_files() {
        let file = HbamFile::open(&path).unwrap();
        let tree = file.key_tree().unwrap();
        for top in &tree.root.children {
            let segment = top.segment.0;
            let subtree = file
                .subtree(&[segment])
                .unwrap()
                .expect("top-level key present");
            assert_eq!(&subtree, top, "{}: subtree lookup", path.display());
            let wire = subtree.to_wire().expect("encodable");
            let back = KeyTree::from_slots(SlotReader::with_path(&wire, vec![KeySegment(segment)]))
                .unwrap();
            match back.node(&[segment]) {
                Some(node) => assert_eq!(
                    node.children,
                    top.children,
                    "{} key {segment:02x?}",
                    path.display()
                ),
                None => assert!(top.children.is_empty() && wire == [0x80, 0x40]),
            }
        }
    }
}

/// Every RID list FileMaker wrote in the samples (each table's record list
/// `[T, 0d]` and every value and word index entry) re-encodes to the same
/// entries under `RidList::entries`, FileMaker Server's writer rule.
#[test]
#[ignore = "requires the git-ignored FileMaker Server extraction"]
fn rid_lists_reencode_as_written() {
    use om_hbam::slot::uint_segment;
    use om_hbam::{OwnedNode, RidList};
    let mut checked = 0;
    for lang in ["en", "de", "fr", "es", "it", "ja"] {
        let file = sample(lang);
        let catalog = file.catalog().unwrap();
        for table in &catalog.tables {
            let table_key = uint_segment(table.id);
            let mut lists: Vec<OwnedNode> = Vec::new();
            if let Some(node) = file.subtree(&[&table_key, &[0x0d]]).unwrap() {
                lists.push(node.to_owned_node());
            }
            if let Some(indexes) = file.subtree(&[&table_key, &[1]]).unwrap() {
                for field in &indexes.children {
                    for index in field.children.iter().filter(|c| c.push) {
                        lists.extend(index.children.iter().map(|k| k.to_owned_node()));
                    }
                }
            }
            for list in &lists {
                let decoded = RidList::from_owned(list).unwrap();
                assert_eq!(decoded.entries(), list.children, "{lang} {}", table.name);
                checked += 1;
            }
        }
    }
    assert!(checked > 100, "{checked} lists");
}

/// The bulk rebuild (hbam-writer.md 7.2) of every shipped file keeps every
/// key and value. (FileMaker starts each chunked value on a fresh page, so
/// its cut points differ from the writer's full pages; any fill is valid.)
#[test]
#[ignore = "requires the git-ignored extracted templates"]
fn rebuild_keeps_every_key_and_value() {
    let mut files = 0;
    for path in all_files() {
        let file = HbamFile::open(&path).unwrap();
        let tree = file.key_tree().unwrap().root.to_owned_node();
        let bytes = om_hbam::writer::rebuild(&tree, file.superblock().raw)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let rebuilt = HbamFile::from_bytes(bytes).unwrap();
        assert_eq!(
            rebuilt.key_tree().unwrap().root.to_owned_node(),
            tree,
            "{}",
            path.display()
        );
        assert_eq!(
            rebuilt.page(1).unwrap().header.next,
            rebuilt.page_count() - 1
        );
        files += 1;
    }
    assert!(files >= 72);
}

/// A new file's page 0, made under the creation name a shipped file's fence
/// holds, is that file's page 0 but for the SCN (file-format.md 1.2).
#[test]
#[ignore = "requires the git-ignored extracted templates"]
fn new_superblock_matches_every_template() {
    let mut files = Vec::new();
    for solution in std::fs::read_dir(MAC_TEMPLATES).expect("templates present") {
        for file in std::fs::read_dir(solution.unwrap().path()).unwrap() {
            let path = file.unwrap().path();
            if path.extension().is_some_and(|e| e == "fmp12") {
                files.push(path);
            }
        }
    }
    assert_eq!(files.len(), 66);
    for path in &files {
        let file = HbamFile::open(path).unwrap();
        let page = file.superblock().raw;
        let fence = file.superblock().identity_fence().unwrap();
        let end = fence.windows(2).position(|w| w == b"/\r").unwrap();
        let name = std::str::from_utf8(&fence[b"EAFI:RC\rR,".len()..end]).unwrap();
        let mut made = om_hbam::new_superblock(name).unwrap();
        made[0x802..0x80a].copy_from_slice(&page[0x802..0x80a]);
        assert!(made == page, "{}", path.display());
    }
}

/// Every shipped file's tree, given node by node as a walk of the tree
/// `HbamFile` reads, streams into a file (`writer::NodeWriter`) as exactly
/// the bytes `rebuild` and the bulk writer before it (`reference`) write
/// from the whole tree (hbam-writer.md 7.2, `docs/server-storage.md` 6.10).
/// Reads whichever of the extractions this host has.
#[test]
#[ignore = "requires the git-ignored extracted templates or FileMaker Server samples"]
fn streamed_files_match_rebuild() {
    use om_hbam::writer::{self, NodeWriter};

    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "fmp12")
                || path.to_string_lossy().ends_with("FMServer_Sample")
            {
                out.push(path);
            }
        }
    }
    fn give<'a>(
        node: &om_hbam::Node<'a>,
        path: &mut Vec<&'a [u8]>,
        writer: &mut NodeWriter<std::fs::File>,
    ) -> om_hbam::Result<usize> {
        path.push(node.segment.0);
        writer.node(path, node.push || !node.children.is_empty(), node.value)?;
        let mut given = 1;
        for child in &node.children {
            given += give(child, path, writer)?;
        }
        path.pop();
        Ok(given)
    }

    let mut files = Vec::new();
    for dir in [WIN_TEMPLATES, MAC_TEMPLATES, FMS_SAMPLES] {
        if Path::new(dir).is_dir() {
            walk(Path::new(dir), &mut files);
        }
    }
    files.sort();
    assert!(!files.is_empty(), "no extracted files");
    let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join("streamed.fmp12");
    let mut nodes = 0;
    for path in &files {
        let ctx = path.display();
        let file = HbamFile::open(path).unwrap();
        let superblock = file.superblock().raw;
        let tree = file.key_tree().unwrap();
        let out = std::fs::File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&target)
            .unwrap();
        let mut writer = NodeWriter::new(out, superblock).unwrap();
        let mut path = Vec::new();
        for child in &tree.root.children {
            nodes += give(child, &mut path, &mut writer).unwrap_or_else(|e| panic!("{ctx}: {e}"));
        }
        writer.finish().unwrap();
        let streamed = std::fs::read(&target).unwrap();
        let owned = tree.root.to_owned_node();
        assert!(
            streamed == writer::rebuild(&owned, superblock).unwrap(),
            "{ctx}"
        );
        assert!(
            streamed == reference::rebuild(&owned, superblock).unwrap(),
            "{ctx}"
        );
    }
    std::fs::remove_file(&target).unwrap();
    println!("{} files, {nodes} nodes", files.len());
}
