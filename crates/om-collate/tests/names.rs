//! Check `name_key` against the name lists FileMaker wrote: every entry of
//! every catalog's name list `[.., 1, 01]` (and a field catalog's list by
//! type `[T, 3, 1, 06]`) must sit under the key computed from the name of
//! the member it names, or under that key and the member's key segment, as
//! FileMaker lists a name several members hold (layout separators, folder
//! ends, a font several catalog entries name; schema-path.md 5.6).
//!
//! Ignored by default: the files are git-ignored. Run with:
//!   cargo test -p om-collate --test names -- --ignored --nocapture

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use om_collate::{name_key, Levels};
use om_hbam::{HbamFile, Node};

/// FileMaker Pro 26's starter files (6 templates in 11 languages), from
/// the extracted application or an installed one.
const SOURCES: [&str; 2] = [
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The member id a name list entry's key string names.
fn member(value: &[u8]) -> Option<&[u8]> {
    let (&len, key) = value.split_first()?;
    (usize::from(len) == key.len()).then_some(key)
}

#[derive(Default)]
struct Tally {
    checked: usize,
    shared: usize,
    mismatches: Vec<String>,
    unkeyed: Vec<String>,
}

fn check_list(
    at: &str,
    path: &str,
    list: &Node,
    members: &Node,
    levels: Levels,
    skip: usize,
    tally: &mut Tally,
) {
    for entry in &list.children {
        let Some(id) = entry.value.and_then(member) else {
            continue;
        };
        let Some(name) = members
            .child(id)
            .and_then(|m| m.child(&[0x10]))
            .and_then(|n| n.text())
            .and_then(Result::ok)
        else {
            continue;
        };
        let stored = entry.segment.0.get(skip..).unwrap_or_default();
        match name_key(&name, levels) {
            Ok(key) if key == stored => tally.checked += 1,
            Ok(key) if [key.as_slice(), id].concat() == stored => tally.shared += 1,
            Ok(key) => tally.mismatches.push(format!(
                "{at} {path} “{name}” member {}: stored {} computed {}",
                hex(id),
                hex(stored),
                hex(&key)
            )),
            Err(why) => tally.unkeyed.push(format!(
                "{at} {path} “{name}” ({why:?}): stored {}",
                hex(stored)
            )),
        }
    }
}

#[test]
#[ignore = "requires FileMaker Pro 26's starter files, git-ignored under re/extracted"]
fn name_lists_hold_the_keys_filemaker_writes() {
    let mut paths = Vec::new();
    // The first source that has them: the same files twice would only
    // count twice.
    for source in SOURCES {
        files(Path::new(source), &mut paths);
        if !paths.is_empty() {
            break;
        }
    }
    assert!(!paths.is_empty(), "no files under {SOURCES:?}");
    let mut tally = Tally::default();
    let mut per_list: BTreeMap<String, usize> = BTreeMap::new();
    for path in &paths {
        let at = path.file_name().unwrap().to_string_lossy().to_string();
        let file = HbamFile::open(path).unwrap_or_else(|e| panic!("{at}: {e}"));
        let tree = file.key_tree().unwrap_or_else(|e| panic!("{at}: {e}"));
        tree.at(&[]).unwrap().walk(&mut |route, node| {
            let (Some(index), Some(members)) = (node.child(&[1]), node.child(&[5])) else {
                return;
            };
            // The path as the specs write it: decimal, `T` for a table.
            let route: Vec<String> = route
                .iter()
                .map(|s| match s.0 {
                    [0x80, ..] if s.0.len() == 2 => "T".to_string(),
                    _ => s.as_uint().map_or_else(|| hex(s.0), |n| n.to_string()),
                })
                .collect();
            let route = route.join(",");
            let before = tally.checked;
            if let Some(names) = index.child(&[1]) {
                check_list(&at, &route, names, members, Levels::Three, 0, &mut tally);
            }
            if route.starts_with("T,") && node.segment.0 == [3] {
                if let Some(typed) = index.child(&[6]) {
                    let path = format!("{route} typed");
                    check_list(&at, &path, typed, members, Levels::Two, 2, &mut tally);
                }
            }
            *per_list.entry(route).or_default() += tally.checked - before;
        });
    }
    println!(
        "{} files: {} entries keyed as FileMaker keys them, {} more of a shared name",
        paths.len(),
        tally.checked,
        tally.shared
    );
    for (list, n) in per_list.iter().filter(|(_, n)| **n > 0) {
        println!("  [{list}]: {n}");
    }
    for line in &tally.unkeyed {
        println!("unkeyed: {line}");
    }
    for line in tally.mismatches.iter().take(80) {
        println!("mismatch: {line}");
    }
    assert!(tally.checked > 0, "no name list entries");
    assert!(
        tally.mismatches.is_empty() && tally.unkeyed.is_empty(),
        "{} mismatches, {} unkeyed",
        tally.mismatches.len(),
        tally.unkeyed.len()
    );
}
