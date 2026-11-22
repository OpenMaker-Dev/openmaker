//! The script catalog index of the shipped templates holds what
//! `om_data::script_catalog` maintains (scripts.md, "The catalog index"):
//! every member has a name list entry under its name key, a member cache
//! entry `07` with its name and member flags, and a place in the display
//! order, and the count and the last issued id cover them. A delete with a
//! move, and a new script with a rename, planned on each file then read
//! back as the Script Workspace asked for them.
//!
//! Ignored by default: the templates exist only on the analysis host. Run with
//!   cargo test -p om-data --test script_catalog_corpus -- --ignored --nocapture

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use om_data::field_catalog::name_key;
use om_data::schema_draft::Stamp;
use om_data::script_catalog::{catalog_digest, plan};
use om_data::{Script, ScriptCatalog, ScriptKind};
use om_hbam::{serial, HbamFile, KeyTree, OwnedNode, SlotReader};

const TEMPLATES: [&str; 2] = [
    "../../re/extracted/mac/FileMaker Pro 26/FileMaker Pro.app/Contents/Resources/DynamicTemplates",
    "/Applications/FileMaker Pro.app/Contents/Resources/DynamicTemplates",
];

fn files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for path in entries.flatten().map(|e| e.path()) {
        if path.is_dir() {
            files(&path, out);
        } else if path.extension().is_some_and(|e| e == "fmp12") {
            out.push(path);
        }
    }
}

fn key_string(segment: &[u8]) -> Vec<u8> {
    let mut out = vec![segment.len() as u8];
    out.extend(segment);
    out
}

fn listed(name: &str) -> Vec<u8> {
    let mut key = name_key(name);
    key.extend([0, 0, 0]);
    key
}

fn value<'a>(node: &'a OwnedNode, path: &[&[u8]]) -> Option<std::borrow::Cow<'a, [u8]>> {
    node.at(path).and_then(OwnedNode::assembled)
}

/// The catalog `[17]` holding `index` and `members`, decoded.
fn decode(index: &OwnedNode, members: &BTreeMap<u64, OwnedNode>) -> ScriptCatalog {
    let wire = OwnedNode::list(
        Vec::new(),
        vec![OwnedNode::list(
            [0x11],
            vec![
                OwnedNode {
                    segment: vec![1],
                    ..index.clone()
                },
                OwnedNode::list([5], members.values().cloned().collect()),
            ],
        )],
    )
    .to_wire()
    .unwrap();
    ScriptCatalog::decode(&KeyTree::from_slots(SlotReader::new(&wire)).unwrap()).unwrap()
}

/// The digest of `catalog`'s members in key order, as FileMaker's open
/// check computes it beside the cache's (scripts.md, "The script
/// catalog's digest"): private flags `f1`, 8 bytes, then the name.
fn members_digest(catalog: &ScriptCatalog) -> [u8; 16] {
    use md5::{Digest, Md5};
    let mut scripts: Vec<&Script> = catalog.scripts.iter().collect();
    scripts.sort_by_key(|s| serial::encode(s.id));
    let mut md5 = Md5::new();
    for script in scripts {
        let flags = script
            .metadata
            .iter()
            .find(|n| n.segment == [0xf1])
            .and_then(|n| n.value.clone())
            .unwrap_or_else(|| vec![0; 8]);
        md5.update(flags);
        for unit in script.name.encode_utf16() {
            md5.update(unit.to_be_bytes());
        }
    }
    md5.finalize().into()
}

/// The members checked, and 1 when the save was refused.
fn check(path: &Path) -> (usize, usize) {
    let at = path.display().to_string();
    let file = HbamFile::open(path).unwrap_or_else(|e| panic!("{at}: {e}"));
    let tree = file.key_tree().unwrap_or_else(|e| panic!("{at}: {e}"));
    let index = tree
        .at(&[0x11, 1])
        .unwrap_or_else(|| panic!("{at}: no script index"))
        .to_owned_node();
    let definitions: BTreeMap<u64, OwnedNode> = tree
        .at(&[0x11, 5])
        .into_iter()
        .flat_map(|members| &members.children)
        .filter_map(|m| Some((serial::decode(m.segment.0)?, m.to_owned_node())))
        .collect();
    let stored = ScriptCatalog::decode(&tree).unwrap_or_else(|e| panic!("{at}: {e:?}"));

    // The index as FileMaker left it. ASCII names are listed under their
    // DUCET key in the English, Dutch and Italian files; the others show
    // other forms (accents add levels; Japanese, Korean and Chinese use
    // other weights).
    let mut ascii_keyed = 0;
    for script in &stored.scripts {
        let segment = serial::encode(script.id);
        let entry = key_string(&segment);
        if value(&index, &[&[1], &listed(&script.name)]).as_deref() == Some(entry.as_slice())
            && script.name.bytes().any(|b| b.is_ascii_alphabetic())
        {
            ascii_keyed += 1;
        }
        let cached = value(&index, &[&[7], &segment, &[0x10]])
            .unwrap_or_else(|| panic!("{at}: no cached name for {}", script.id));
        assert_eq!(
            Some(&cached),
            value(&definitions[&script.id], &[&[0x10]]).as_ref(),
            "{at}: cached name of “{}”",
            script.name
        );
        assert_eq!(
            value(&index, &[&[7], &segment, &[0xf0]]),
            value(&definitions[&script.id], &[&[0xf0]]),
            "{at}: cached flags of “{}”",
            script.name
        );
    }
    let count = value(&index, &[&[4]]).unwrap();
    assert_eq!(
        count.as_ref(),
        (stored.scripts.len() as u32).to_be_bytes(),
        "{at}: count"
    );
    let order = index.child(&[3]).unwrap();
    assert_eq!(order.children.len(), stored.scripts.len(), "{at}: order");
    for (i, entry) in order.children.iter().enumerate() {
        assert_eq!(
            entry.segment,
            (i as u32 + 1).to_be_bytes(),
            "{at}: position"
        );
    }

    // `08` is the digest of the member cache, and of the members.
    let stored_digest = value(&index, &[&[8]]).map(|v| v.into_owned());
    assert_eq!(
        catalog_digest(&index).map(|d| d.to_vec()),
        stored_digest,
        "{at}: the cache's digest"
    );
    assert_eq!(
        Some(members_digest(&stored).to_vec()),
        stored_digest,
        "{at}: the members' digest"
    );

    // The first script deleted and the last moved to the top: no name is
    // written, so every file takes it whatever its language.
    let scripts_at: Vec<usize> = stored
        .scripts
        .iter()
        .enumerate()
        .filter(|(_, s)| s.kind == ScriptKind::Script)
        .map(|(i, _)| i)
        .collect();
    let mut requested = stored.clone();
    let last = requested.scripts.remove(*scripts_at.last().unwrap());
    requested.scripts.insert(0, last);
    let first_id = stored.scripts[scripts_at[0]].id;
    requested.scripts.retain(|s| s.id != first_id);
    let save = plan(
        Some(&index),
        &stored,
        &definitions,
        &requested,
        &Stamp {
            user: "Tester".into(),
            account: "Admin".into(),
            time: vec![0x9f, 0x91, 0x03, 0x80, 0x05, 0x52, 0x17, 0x80],
        },
        &mut || [7; 16],
    )
    .unwrap_or_else(|e| panic!("{at}: delete and move: {}", e.message))
    .unwrap();
    assert_eq!(save.deleted, [serial::encode(first_id)], "{at}");
    let mut members = definitions.clone();
    members.insert(
        first_id,
        OwnedNode::list(serial::encode(first_id), Vec::new()),
    );
    let saved = decode(&save.index, &members);
    assert_eq!(
        saved.scripts.iter().map(|s| s.id).collect::<Vec<_>>(),
        requested.scripts.iter().map(|s| s.id).collect::<Vec<_>>(),
        "{at}: catalog after the delete and move"
    );
    assert_eq!(
        value(&save.index, &[&[4]]).as_deref(),
        Some(&(stored.scripts.len() as u32 - 1).to_be_bytes()[..]),
        "{at}: count"
    );
    let digest = value(&save.index, &[&[8]]).map(|v| v.into_owned());
    assert_eq!(
        digest,
        Some(members_digest(&saved).to_vec()),
        "{at}: digest"
    );
    assert_eq!(
        digest,
        catalog_digest(&save.index).map(|d| d.to_vec()),
        "{at}: digest"
    );
    assert_ne!(digest, stored_digest, "{at}: the deleted member's digest");

    // A new script after the first script, and that script renamed.
    let first = stored
        .scripts
        .iter()
        .position(|s| s.kind == ScriptKind::Script)
        .unwrap();
    let mut requested = stored.clone();
    requested.scripts[first].name = "Renamed by OpenMaker".into();
    requested.scripts.insert(
        first + 1,
        Script {
            id: 9999,
            name: "New by OpenMaker".into(),
            kind: ScriptKind::Script,
            ..Script::default()
        },
    );
    let by = Stamp {
        user: "Tester".into(),
        account: "Admin".into(),
        time: vec![0x9f, 0x91, 0x03, 0x80, 0x05, 0x52, 0x17, 0x80],
    };
    let planned = plan(
        Some(&index),
        &stored,
        &definitions,
        &requested,
        &by,
        &mut || [7; 16],
    );
    let ascii_named = stored.scripts.iter().any(|s| {
        s.kind == ScriptKind::Script
            && s.name.bytes().any(|b| b.is_ascii_alphabetic())
            && s.name.is_ascii()
    });
    let save = match planned {
        Ok(Some(save)) => save,
        Ok(None) => panic!("{at}: nothing planned"),
        // Name list keys are the same in every file (#274): a new ASCII
        // name is listed in every starter, the Japanese, Korean and
        // Chinese ones, which hold no ASCII script name, included.
        Err(e) => panic!(
            "{at}: {} ({ascii_keyed} ASCII-keyed, ASCII-named scripts: {ascii_named})",
            e.message
        ),
    };
    let mut members = definitions.clone();
    for member in &save.members {
        members.insert(serial::decode(&member.segment).unwrap(), member.clone());
    }
    let saved = decode(&save.index, &members);
    let id = save.assigned[&9999];
    let names: Vec<(u64, &str)> = saved
        .scripts
        .iter()
        .map(|s| (s.id, s.name.as_str()))
        .collect();
    let mut expected: Vec<(u64, &str)> = requested
        .scripts
        .iter()
        .map(|s| (if s.id == 9999 { id } else { s.id }, s.name.as_str()))
        .collect();
    expected.dedup();
    assert_eq!(names, expected, "{at}: catalog after the save");
    for (key, name) in [
        (serial::encode(id), "New by OpenMaker"),
        (
            serial::encode(stored.scripts[first].id),
            "Renamed by OpenMaker",
        ),
    ] {
        assert_eq!(
            value(&save.index, &[&[1], &listed(name)]).as_deref(),
            Some(key_string(&key).as_slice()),
            "{at}: name list entry of “{name}”"
        );
    }
    assert!(
        value(&save.index, &[&[1], &listed(&stored.scripts[first].name)]).is_none(),
        "{at}: old name's entry"
    );
    assert_eq!(
        value(&save.index, &[&[8]]).map(|v| v.into_owned()),
        Some(members_digest(&saved).to_vec()),
        "{at}: digest after the save"
    );
    for key in [[0x20u8], [0xd8]] {
        assert_eq!(
            index.child(&key),
            save.index.child(&key),
            "{at}: {key:02x?}"
        );
    }
    (stored.scripts.len(), 0)
}

#[test]
#[ignore = "requires the shipped templates under re/extracted"]
fn the_templates_script_indexes_are_what_a_save_maintains() {
    let mut paths = Vec::new();
    if let Some(dir) = TEMPLATES.iter().map(Path::new).find(|d| d.is_dir()) {
        files(dir, &mut paths);
    }
    assert!(!paths.is_empty(), "no template files found");
    let (mut members, mut refused) = (0, 0);
    for path in &paths {
        let (m, r) = check(path);
        members += m;
        refused += r;
    }
    println!(
        "{} files: {members} members saved over, {refused} files refused",
        paths.len()
    );
}
