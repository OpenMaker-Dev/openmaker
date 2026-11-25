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
    "[11, 05, *, 13]",
    "[19, 05, *, 02]",
    "[19, 05, *, 03]",
    "[41, 05, *, 02]",
];

/// Whether `node` holds a value stored as a header and pieces
/// (file-format.md 3.6), which edit tokens now hash as the value it
/// joins to.
fn holds_split_value(node: &OwnedNode) -> bool {
    node.children
        .iter()
        .any(|c| design_paths::split_value(c).is_some() || holds_split_value(c))
}

/// How an object's edit token compares with the old code's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Old {
    Equal,
    /// It holds a split value, now hashed as the value it joins to.
    Split,
}

/// Compare a script's, layout's or value list's edit token with the old
/// code's: equal unless a split value explains the difference; and its
/// legacy token, which a host still accepts, always equal.
fn against_oracle(object: &design_paths::Object<'_>) -> Old {
    let (old, legacy) = match object.kind() {
        Kind::Script => (
            oracle::script(object.node()),
            design_paths::legacy_script_form_revision(object.node()),
        ),
        _ => (
            oracle::catalog(object.node()),
            design_paths::legacy_catalog_revision(object.node()),
        ),
    };
    assert_eq!(legacy, old);
    let token = object.edit_token();
    if holds_split_value(object.node()) {
        assert_ne!(token, old);
        Old::Split
    } else {
        assert_eq!(token, old, "{:?} {:02x?}", object.kind(), object.key());
        Old::Equal
    }
}

#[test]
fn scripts_layouts_and_value_lists_keep_the_old_revisions_but_for_secrets_and_split_values() {
    let leaf = |key: &[u8], value: &[u8]| OwnedNode::leaf(key.to_vec(), value.to_vec());
    let stamp = || {
        vec![
            leaf(&[0xfc], &[1, 2]),
            leaf(&[0xfc, 0x01], b"user"),
            leaf(&[0xfc, 0x03], &[8; 9]),
        ]
    };
    // A script whose step data holds a stamp-like key and an empty list; a
    // script with Re-Login (step 138, data key 1) and its password in slot
    // 1; a layout with a stamp on an attribute record, and one with a
    // chunked object store; a value list.
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
    let mut relogin = vec![0x02, 0x01, 0x01, 0, 0, 0];
    relogin.extend([0; 14]);
    relogin.extend(138i16.to_be_bytes());
    relogin.extend([0; 6]);
    let mut signing_in = vec![
        leaf(&[0x04], &relogin),
        OwnedNode::list(
            vec![0x05],
            vec![OwnedNode::list(
                vec![0x01],
                vec![OwnedNode::list(vec![0, 1], vec![leaf(&[0x05], b"pw")])],
            )],
        ),
        leaf(&[0x10], b"Sign in"),
    ];
    signing_in.extend(stamp());
    let mut layout = vec![
        leaf(&[0x02], &[0x10, 0x82]),
        OwnedNode::list(
            vec![0x05],
            vec![OwnedNode::list(
                vec![0x01],
                vec![leaf(&[0x10], b""), leaf(&[0xfc, 0x02], b"Admin")],
            )],
        ),
        leaf(&[0x07], &[7; 900]),
        leaf(&[0x10], b"Form"),
        leaf(&[0x16], &[0, 0]),
    ];
    layout.extend(stamp());
    let mut large = layout.clone();
    large[2] = OwnedNode::list(
        vec![0x07],
        vec![
            leaf(&[0x00], &1500u32.to_be_bytes()),
            leaf(&[0x01], &[7; 1000]),
            leaf(&[0x02], &[7; 500]),
        ],
    );
    let mut list = vec![leaf(&[0x03], b"A\rB"), leaf(&[0x10], b"Kinds")];
    list.extend(stamp());
    let mut root = OwnedNode::default();
    root.put(&[&[0x11], &[5], &[3]], OwnedNode::list(vec![], script));
    root.put(&[&[0x11], &[5], &[4]], OwnedNode::list(vec![], signing_in));
    root.put(&[&[0x04], &[5], &[2]], OwnedNode::list(vec![], layout));
    root.put(&[&[0x04], &[5], &[3]], OwnedNode::list(vec![], large));
    root.put(&[&[0x21], &[5], &[1]], OwnedNode::list(vec![], list));

    let compared: Vec<(Kind, Old)> = design_paths::objects(&root)
        .iter()
        .map(|object| (object.kind(), against_oracle(object)))
        .collect();
    assert_eq!(
        compared,
        [
            (Kind::Layout, Old::Equal),
            (Kind::Layout, Old::Split),
            (Kind::Script, Old::Equal),
            (Kind::Script, Old::Equal),
            (Kind::ValueList, Old::Equal),
        ]
    );
}

fn tree(path: &Path) -> (HbamFile, OwnedNode) {
    let file = HbamFile::open(path).expect("a readable file");
    let root = file.key_tree().expect("a key tree").root.to_owned_node();
    (file, root)
}

type Id = (Kind, bool, Option<Vec<u8>>, Vec<u8>);

fn revisions(root: &OwnedNode) -> BTreeMap<Id, [u8; 32]> {
    design_paths::objects(root)
        .iter()
        .map(|o| {
            (
                (
                    o.kind(),
                    o.is_catalog(),
                    o.table().map(<[u8]>::to_vec),
                    o.key().to_vec(),
                ),
                o.edit_token(),
            )
        })
        .collect()
}

#[test]
#[ignore = "requires the git-ignored extracted templates in re/extracted"]
fn every_path_is_grouped_every_member_accounted_for_and_revisions_kept() {
    let files = files();
    assert!(!files.is_empty(), "no template folder under re/extracted");
    let mut total = Report::default();
    let mut objects: BTreeMap<(Kind, bool), usize> = BTreeMap::new();
    let mut members: BTreeMap<(String, MemberIs), usize> = BTreeMap::new();
    let mut compared: BTreeMap<(Kind, Old), usize> = BTreeMap::new();
    let mut with_secrets: BTreeMap<Kind, usize> = BTreeMap::new();
    // Raw tokens over whole subtrees: fields_revision, the layout and
    // script catalogs' indexes, as before and as now.
    let mut raw = (0usize, 0usize);
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
                        listed.iter().any(|o| !o.is_catalog()
                            && o.kind() == catalog.kind.unwrap()
                            && o.key() == member.segment
                            && o.table() == table.as_deref()),
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

        // Scripts, layouts and value lists keep the revisions the old code
        // gave them, but where a Secret path or a split value explains the
        // difference.
        for object in &listed {
            *objects
                .entry((object.kind(), object.is_catalog()))
                .or_default() += 1;
            if !object.secrets().is_empty() {
                *with_secrets.entry(object.kind()).or_default() += 1;
            }
            if !object.is_catalog()
                && matches!(object.kind(), Kind::Script | Kind::Layout | Kind::ValueList)
            {
                *compared
                    .entry((object.kind(), against_oracle(object)))
                    .or_default() += 1;
            }
        }
        let mut tokens: Vec<&OwnedNode> = root
            .children
            .iter()
            .filter(|t| Seg::Table.matches(&t.segment))
            .filter_map(|t| t.child(&[3]))
            .collect();
        tokens.extend(root.at(&[&[4], &[1]]));
        tokens.extend(root.at(&[&[0x11], &[1]]));
        for node in tokens {
            raw.0 += 1;
            if design_paths::catalog_revision(node) != oracle::catalog(node) {
                assert!(holds_split_value(node), "{ctx}");
                raw.1 += 1;
            }
        }

        // A rebuilt file, read back, has the same revisions.
        let revisions_before = revisions(&root);
        let bytes = om_hbam::writer::rebuild(&root, file.superblock().raw).expect("a rebuild");
        let reread = HbamFile::from_bytes(bytes).expect("a rebuilt file");
        let again = reread.key_tree().expect("a key tree").root.to_owned_node();
        assert_eq!(revisions(&again), revisions_before, "{ctx}");
    }

    println!("{} files", files.len());
    print!("{total}");
    println!("objects by kind (catalogs as a whole separately):");
    for ((kind, catalog), count) in &objects {
        let what = if *catalog { " catalog" } else { "" };
        println!("  {count:>9}  {}{what}", kind.name());
    }
    println!("objects holding a Secret path: {with_secrets:?}");
    println!("catalog members that are no object:");
    for ((catalog, is), count) in &members {
        println!("  {count:>9}  {catalog}: {is:?}");
    }
    println!("revisions against the old code's: {compared:?}");
    println!(
        "fields_revision, layouts_revision and the script index: {} tokens, {} changed (split values)",
        raw.0, raw.1
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
fn the_storage_design_shows_the_rule_tables() {
    let doc = std::fs::read_to_string("../../docs/server-storage.md").expect("the storage design");
    for (name, table) in [
        ("markdown_table", design_paths::markdown_table()),
        ("markdown_secrets", design_paths::markdown_secrets()),
    ] {
        assert!(
            doc.contains(&table),
            "docs/server-storage.md section 4 must show design_paths::{name}() as it is"
        );
    }
}

/// Per kind: the objects with and without a design revision, why those
/// without have none (the shapes of their paths in no group, or a Secret
/// path that could not be identified), and the objects with no decoded
/// name, which a comparison without a base can never match.
#[test]
#[ignore = "requires the git-ignored extracted templates in re/extracted"]
fn design_revisions_and_names_per_kind() {
    let files = files();
    assert!(!files.is_empty(), "no template folder under re/extracted");
    // (kind, catalog) -> (with, without, without a name)
    let mut table: BTreeMap<(Kind, bool), (usize, usize, usize)> = BTreeMap::new();
    let mut why: BTreeMap<(Kind, bool, String), usize> = BTreeMap::new();
    for path in &files {
        let (_, root) = tree(path);
        let objects = design_paths::objects(&root);
        // A file compared with itself matches every object, those with no
        // name by their content, and finds them all the same.
        let itself = design_paths::compare(&root, &root, None).unwrap();
        assert_eq!(itself.matched.len(), objects.len());
        assert!(itself.possible_matches.is_empty());
        assert!(itself.only_in_source.is_empty() && itself.only_in_target.is_empty());
        assert!(itself
            .matched
            .iter()
            .all(|m| m.difference == design_paths::Difference::Same));
        for object in &objects {
            if !object.is_catalog() && object.design_name().is_none() {
                table
                    .entry((object.kind(), object.is_catalog()))
                    .or_default()
                    .2 += 1;
            }
            let entry = table
                .entry((object.kind(), object.is_catalog()))
                .or_default();
            if object.design_revision().is_some() {
                entry.0 += 1;
                continue;
            }
            entry.1 += 1;
            let mut reasons = object.unclassified();
            reasons.extend(
                object
                    .secrets()
                    .into_iter()
                    .map(|(_, holds)| format!("Secret: {holds}")),
            );
            assert!(
                !reasons.is_empty(),
                "{:?} {:02x?}",
                object.kind(),
                object.key()
            );
            reasons.sort();
            reasons.dedup();
            for reason in reasons {
                *why.entry((object.kind(), object.is_catalog(), reason))
                    .or_default() += 1;
            }
        }
    }
    println!("| Objects | With a design revision | Without | No decoded name |");
    println!("|---|---|---|---|");
    for ((kind, catalog), (with, without, unnamed)) in &table {
        let what = if *catalog { " catalog" } else { "" };
        println!("| {}{what} | {with} | {without} | {unnamed} |", kind.name());
    }
    println!("why:");
    for ((kind, catalog, reason), count) in &why {
        println!(
            "  {count:>5}  {}{} {reason}",
            kind.name(),
            if *catalog { " catalog" } else { "" }
        );
    }
    let count =
        |kind: Kind, catalog: bool| table.get(&(kind, catalog)).copied().unwrap_or_default();
    // Scripts hold the unexplained `13`; the catalogs given rules by
    // inference now have revisions.
    assert_eq!(count(Kind::Script, false).0, 0);
    for kind in [
        Kind::PrivilegeSet,
        Kind::ExtendedPrivilege,
        Kind::Font,
        Kind::ValueList,
        Kind::CustomMenuSet,
    ] {
        assert_eq!(count(kind, true).1, 0, "{kind:?}");
    }
    assert_eq!(count(Kind::Field, false).1, 0);
}

/// Two language versions of one starter solution compared, without a base
/// and with the English one as the base, and each with itself: the counts
/// per class, a check that the classes say something sensible.
#[test]
#[ignore = "requires the git-ignored extracted templates in re/extracted"]
fn two_language_versions_compare_class_by_class() {
    let files = files();
    let find = |solution: &str, language: &str| {
        files
            .iter()
            .find(|p| p.ends_with(format!("{solution}/{language}.fmp12")))
            .cloned()
    };
    let (Some(en), Some(de)) = (find("Tasks", "en"), find("Tasks", "de")) else {
        panic!("no Tasks starter files under re/extracted");
    };
    let print = |label: &str, c: &design_paths::Comparison| {
        let mut classes: BTreeMap<&str, usize> = BTreeMap::new();
        for m in &c.matched {
            *classes.entry(m.difference.name()).or_default() += 1;
        }
        let by_agreement = |a| c.possible_matches.iter().filter(|p| p.agrees == a).count();
        let without = c
            .matched
            .iter()
            .filter(|m| m.source_revision.is_none() || m.target_revision.is_none())
            .count();
        println!(
            "{label}: matched {} ({without} without a design revision) {classes:?}; possible matches by id {}, by name {}; only in source {}, only in target {}; changed by {:?}",
            c.matched.len(),
            by_agreement(design_paths::Agreement::Id),
            by_agreement(design_paths::Agreement::Name),
            c.only_in_source.len(),
            c.only_in_target.len(),
            {
                let mut by: BTreeMap<String, usize> = BTreeMap::new();
                for m in &c.matched {
                    if let Some(base) = &m.base {
                        *by.entry(format!("{:?}", base.changed_by())).or_default() += 1;
                    }
                }
                by
            }
        );
    };
    let same = design_paths::compare_files(&en, &en, None).unwrap();
    print("Tasks en with itself", &same);
    assert!(!same.matched.is_empty() && same.possible_matches.is_empty());
    assert!(same
        .matched
        .iter()
        .all(|m| m.difference == design_paths::Difference::Same));
    let without = design_paths::compare_files(&en, &de, None).unwrap();
    print("Tasks en and de, no base", &without);
    let with = design_paths::compare_files(&en, &de, Some(&en)).unwrap();
    print("Tasks en and de, en as the base", &with);
    // With English as the base every object both files have is matched,
    // and only the German side changed any.
    assert!(!with.matched.is_empty() && !without.matched.is_empty());
    assert!(with.possible_matches.is_empty());
    assert!(with.matched.iter().all(|m| {
        let base = m.base.as_ref().unwrap();
        base.source == design_paths::Difference::Same && base.target == m.difference
    }));
    assert!(with.matched.len() >= without.matched.len());
}
