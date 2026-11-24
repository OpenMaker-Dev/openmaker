//! The path groups, objects and revisions of `om_hbam::design_paths` over
//! the FileMaker files the template tests read.
//!
//! Ignored by default: the templates are git-ignored extractions. Run with
//!   cargo test -p om-hbam --test design_paths -- --ignored --nocapture
//! It reads whichever of the template folders exist.
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use om_hbam::design_paths::{self, Group, Kind, MemberIs, Report, Seg};
use om_hbam::{HbamFile, OwnedNode};

const DIRS: [&str; 3] = [
    "../../re/extracted/win/msi/PFiles64/FileMaker/FileMaker Pro/DynamicTemplates",
    "../../re/extracted/mac/FileMaker Pro 26/FileMaker Pro.app/Contents/Resources/DynamicTemplates",
    "../../re/extracted/fms/deb/opt/FileMaker/FileMaker Server/Data/Databases/.Sample",
];

fn files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("a readable directory") {
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
    for dir in DIRS.iter().map(Path::new).filter(|d| d.is_dir()) {
        walk(dir, &mut out);
    }
    out.sort();
    out
}

/// The revisions as `om-protocol` computed them before
/// `om_hbam::design_paths` existed, kept here as the oracle: a script's
/// over its member leaving out its own stamp, with list flags; a layout's
/// over its member leaving out every stamp, without list flags.
mod oracle {
    use om_hbam::OwnedNode;

    fn is_stamp(segment: &[u8]) -> bool {
        [&[0xfc][..], &[0xfc, 0x01], &[0xfc, 0x02], &[0xfc, 0x03]].contains(&segment)
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Stamps {
        Own,
        Kept,
        Everywhere,
    }

    fn canonical_with(node: &OwnedNode, stamps: Stamps, out: &mut Vec<u8>) {
        fn bytes(out: &mut Vec<u8>, b: &[u8]) {
            out.extend_from_slice(&(b.len() as u64).to_be_bytes());
            out.extend_from_slice(b);
        }
        bytes(out, &node.segment);
        if stamps != Stamps::Everywhere {
            out.push(u8::from(node.push));
        }
        match &node.value {
            Some(value) => {
                out.push(1);
                bytes(out, value);
            }
            None => out.push(0),
        }
        let children: Vec<&OwnedNode> = node
            .children
            .iter()
            .filter(|c| stamps == Stamps::Kept || !is_stamp(&c.segment))
            .collect();
        out.extend_from_slice(&(children.len() as u64).to_be_bytes());
        let below = match stamps {
            Stamps::Own | Stamps::Kept => Stamps::Kept,
            Stamps::Everywhere => Stamps::Everywhere,
        };
        for child in children {
            canonical_with(child, below, out);
        }
    }

    pub fn script(member: &OwnedNode) -> [u8; 32] {
        let mut bytes = Vec::new();
        canonical_with(member, Stamps::Own, &mut bytes);
        om_crypto::sha256(&bytes)
    }

    pub fn catalog(node: &OwnedNode) -> [u8; 32] {
        let mut bytes = Vec::new();
        canonical_with(node, Stamps::Everywhere, &mut bytes);
        om_crypto::sha256(&bytes)
    }
}

/// The shapes the shipped files hold that no rule covers: keys no spec in
/// `docs/re/` explains (file-format.md 3.7).
const UNCLASSIFIED: &[&str] = &[
    "[04, 05, *, 16]",
    "[06, 01, 20]",
    "[11, 05, *, 13]",
    "[17, 02, 01, 07, …]",
    "[17, 03, 01, 07, …]",
    "[19, 01, 07, …]",
    "[19, 05, *, 02]",
    "[19, 05, *, 03]",
    "[21, 01, 06, …]",
    "[21, 01, 07, …]",
    "[40, 01, 07, …]",
    "[40, 01, 18]",
    "[40, 01, 20]",
    "[41, 01, 07, …]",
    "[41, 05, *, 02]",
];

#[test]
fn scripts_layouts_and_value_lists_keep_the_revisions_the_old_code_gave() {
    let leaf = |key: &[u8], value: &[u8]| OwnedNode::leaf(key.to_vec(), value.to_vec());
    let stamp = || {
        vec![
            leaf(&[0xfc], &[1, 2]),
            leaf(&[0xfc, 0x01], b"user"),
            leaf(&[0xfc, 0x03], &[8; 9]),
        ]
    };
    // A script whose step data holds a stamp-like key and an empty list,
    // a layout with a chunked object store and a stamp on an attribute
    // record, a value list.
    let mut script = vec![
        leaf(&[0x04], &[1; 28]),
        OwnedNode::list(
            vec![0x05],
            vec![OwnedNode::list(
                vec![0x01],
                vec![leaf(&[0xfc], &[1, 1]), OwnedNode::list(vec![0x02], vec![])],
            )],
        ),
        leaf(&[0x10], b"Greeting"),
        leaf(&[0x13], &[0; 4]),
    ];
    script.extend(stamp());
    let mut layout = vec![
        leaf(&[0x02], &[0x10, 0x82]),
        OwnedNode::list(
            vec![0x05],
            vec![OwnedNode::list(
                vec![0x01],
                vec![leaf(&[0x10], b""), leaf(&[0xfc, 0x02], b"Admin")],
            )],
        ),
        OwnedNode::list(
            vec![0x07],
            vec![
                leaf(&[0x00], &1500u32.to_be_bytes()),
                leaf(&[0x01], &[7; 1000]),
                leaf(&[0x02], &[7; 500]),
            ],
        ),
        leaf(&[0x10], b"Form"),
        leaf(&[0x16], &[0, 0]),
    ];
    layout.extend(stamp());
    let mut list = vec![leaf(&[0x03], b"A\rB"), leaf(&[0x10], b"Kinds")];
    list.extend(stamp());
    let mut root = OwnedNode::default();
    root.put(&[&[0x11], &[5], &[3]], OwnedNode::list(vec![], script));
    root.put(&[&[0x04], &[5], &[2]], OwnedNode::list(vec![], layout));
    root.put(&[&[0x21], &[5], &[1]], OwnedNode::list(vec![], list));

    let objects = design_paths::objects(&root);
    assert_eq!(objects.len(), 3);
    for object in &objects {
        let old = match object.kind {
            Kind::Script => oracle::script(object.member),
            _ => oracle::catalog(object.member),
        };
        assert_eq!(object.revision(), old, "{:?}", object.kind);
        assert_eq!(
            design_paths::revision_of(object.kind, None, object.member),
            old
        );
    }
}

fn tree(path: &Path) -> (HbamFile, OwnedNode) {
    let file = HbamFile::open(path).expect("a readable file");
    let root = file.key_tree().expect("a key tree").root.to_owned_node();
    (file, root)
}

#[test]
#[ignore = "requires the git-ignored extracted templates in re/extracted"]
fn every_path_is_grouped_every_member_accounted_for_and_revisions_kept() {
    let files = files();
    assert!(!files.is_empty(), "no template folder under re/extracted");
    let mut total = Report::default();
    let mut objects: BTreeMap<Kind, usize> = BTreeMap::new();
    let mut members: BTreeMap<(String, MemberIs), usize> = BTreeMap::new();
    let mut compared = (0usize, 0usize);
    for path in &files {
        let ctx = path.display();
        let (file, root) = tree(path);
        let report = design_paths::report(&root);
        for (group, count) in &report.groups {
            *total.groups.entry(*group).or_default() += count;
        }
        for (shape, count) in &report.unclassified {
            *total.unclassified.entry(shape.clone()).or_default() += count;
        }
        for (rule, count) in &report.hits {
            *total.hits.entry(*rule).or_default() += count;
        }
        total.structure += report.structure;
        for member in &report.members {
            assert_ne!(member.is, MemberIs::Unknown, "{ctx}: {member:?}");
            *members
                .entry((member.catalog.clone(), member.is))
                .or_default() += 1;
        }

        // Every non-empty member of a Design catalog is an object.
        let listed = design_paths::objects(&root);
        for catalog in design_paths::CATALOGS
            .iter()
            .filter(|c| c.group == Group::Design)
        {
            let tables: Vec<Option<Vec<u8>>> = if catalog.per_table() {
                root.children
                    .iter()
                    .filter(|t| Seg::Table.matches(&t.segment))
                    .map(|t| Some(t.segment.clone()))
                    .collect()
            } else {
                vec![None]
            };
            for table in tables {
                let mut at = catalog.at(table.as_deref());
                at.push(vec![5]);
                let segments: Vec<&[u8]> = at.iter().map(Vec::as_slice).collect();
                let Some(list) = root.at(&segments) else {
                    continue;
                };
                for member in list.children.iter().filter(|m| !m.children.is_empty()) {
                    assert!(
                        listed.iter().any(|o| o.kind == catalog.kind.unwrap()
                            && o.key == member.segment
                            && o.table == table),
                        "{ctx}: {} member {:02x?} is no object",
                        catalog.name,
                        member.segment
                    );
                }
            }
        }

        // The top-level keys taken for tables and occurrences are the
        // file's tables and occurrences.
        let ids = |catalog: u8| -> BTreeSet<Vec<u8>> {
            root.at(&[&[3], &[catalog], &[5]])
                .map(|n| n.children.iter().map(|c| c.segment.clone()).collect())
                .unwrap_or_default()
        };
        let (tables, occurrences) = (ids(0x10), ids(0x11));
        for top in &root.children {
            if Seg::Table.matches(&top.segment) {
                assert!(tables.contains(&top.segment), "{ctx}: {:02x?}", top.segment);
            }
            if Seg::Occurrence.matches(&top.segment) {
                assert!(
                    occurrences.contains(&top.segment),
                    "{ctx}: {:02x?}",
                    top.segment
                );
            }
        }

        // Scripts and layouts keep the revisions the old code gave them.
        let mut revisions = BTreeMap::new();
        for object in &listed {
            *objects.entry(object.kind).or_default() += 1;
            let revision = object.revision();
            match object.kind {
                Kind::Script => {
                    assert_eq!(revision, oracle::script(object.member), "{ctx}");
                    compared.0 += 1;
                }
                Kind::Layout | Kind::ValueList => {
                    assert_eq!(revision, oracle::catalog(object.member), "{ctx}");
                    compared.1 += 1;
                }
                _ => {}
            }
            revisions.insert(
                (object.kind, object.table.clone(), object.key.clone()),
                revision,
            );
        }

        // A rebuilt file, read back, has the same revisions.
        let bytes = om_hbam::writer::rebuild(&root, file.superblock().raw).expect("a rebuild");
        let reread = HbamFile::from_bytes(bytes).expect("a rebuilt file");
        let again = reread.key_tree().expect("a key tree").root.to_owned_node();
        let after: BTreeMap<_, _> = design_paths::objects(&again)
            .iter()
            .map(|o| ((o.kind, o.table.clone(), o.key.clone()), o.revision()))
            .collect();
        assert_eq!(after, revisions, "{ctx}");
    }

    println!("{} files", files.len());
    print!("{total}");
    println!("objects by kind:");
    for (kind, count) in &objects {
        println!("  {count:>9}  {}", kind.name());
    }
    println!("catalog members that are no object:");
    for ((catalog, is), count) in &members {
        println!("  {count:>9}  {catalog}: {is:?}");
    }
    println!(
        "revisions equal to the old code's: {} scripts, {} layouts and value lists",
        compared.0, compared.1
    );
    let rules = design_paths::rules();
    let unhit: Vec<String> = (0..rules.len())
        .filter(|i| !total.hits.contains_key(i))
        .map(|i| rules[i].pattern_text())
        .collect();
    println!(
        "rules hit: {} of {}; not hit: {}",
        rules.len() - unhit.len(),
        rules.len(),
        unhit.join(" ")
    );

    let shapes: Vec<&str> = total.unclassified.keys().map(String::as_str).collect();
    assert_eq!(shapes, UNCLASSIFIED);
}

#[test]
fn the_storage_design_shows_the_rule_table() {
    let doc = std::fs::read_to_string("../../docs/server-storage.md").expect("the storage design");
    assert!(
        doc.contains(&design_paths::markdown_table()),
        "docs/server-storage.md section 4 must show design_paths::markdown_table() as it is"
    );
}
