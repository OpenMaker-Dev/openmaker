//! Replay the op86 test vectors recorded against FileMaker Server
//! (`re/work/query/vectors/live-*`, git-ignored) through om-query on a copy
//! of the same file taken right after, and compare the replies byte for byte.
//!
//! Run: cargo test -p om-query --test vectors -- --ignored --nocapture

use std::path::Path;

const VECTORS: &str = "../../re/work/query/vectors";
const TEST_PROJECT: &str = "../../re/work/omserver/testproject-live-1234.fmp12";

/// The reply slots of a recorded op86 result stack (its last 23 bytes are
/// `u32 N` and the resume block).
fn reply_slots(result: &[u8]) -> &[u8] {
    let trailer = result.len() - 23;
    let n = u32::from_be_bytes(result[trailer..trailer + 4].try_into().unwrap()) as usize;
    assert_eq!(n, trailer);
    &result[..n]
}

#[test]
#[ignore = "requires the git-ignored vectors and test project copy"]
fn live_vectors_match_filemaker_server() {
    let file = om_hbam::HbamFile::open(TEST_PROJECT).unwrap();
    let mut dirs: Vec<_> = std::fs::read_dir(VECTORS)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("live-")
        })
        .collect();
    dirs.sort();
    let (mut matched, mut differ, mut unsupported) = (Vec::new(), Vec::new(), Vec::new());
    for dir in &dirs {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let read = |f: &str| std::fs::read(Path::new(dir).join(f)).unwrap();
        let (txn, program, reply) = (read("txn.bin"), read("program.bin"), read("reply.bin"));
        let file_id = u32::from_be_bytes(txn[12..16].try_into().unwrap());
        let context = om_query::Context {
            file_id,
            connection_key: None,
        };
        let outcome = om_query::execute(&file, &program, &context);
        match outcome.error {
            Some(
                om_query::QueryError::Unsupported(..) | om_query::QueryError::Unimplemented(_),
            ) => {
                unsupported.push(format!("{name}: {}", outcome.error.unwrap()));
            }
            _ if outcome.slots == reply_slots(&reply) => matched.push(name),
            _ => differ.push(name),
        }
    }
    println!("matched {}: {matched:?}", matched.len());
    println!("differ {}: {differ:?}", differ.len());
    println!("not run {}:", unsupported.len());
    for u in &unsupported {
        println!("  {u}");
    }
    // Expected differences, by design:
    // - jp_badkey: the replay skips the access-key check (the recorded
    //   session's connection key is not kept); the capture test checks keys
    //   against FileMaker Pro's own programs, and a unit test checks rejection.
    // - getkey_node: FileMaker Server returns an opaque server position for a
    //   node, which another server cannot reproduce.
    differ.retain(|name| !matches!(name.as_str(), "live-jp_badkey" | "live-getkey_node"));
    assert!(differ.is_empty(), "replies differ from FileMaker Server");
    assert!(matched.len() >= 64, "{} matched", matched.len());
}

/// The CreateValueListEx (op 19) probes (`vl-*`), run against the copy of
/// test project taken right after them.
#[test]
#[ignore = "requires the git-ignored vectors and test project copy"]
fn value_lists_match_filemaker_server() {
    let file = om_hbam::HbamFile::open("../../re/work/omserver/testproject-live-vl.fmp12").unwrap();
    let mut dirs: Vec<_> = std::fs::read_dir(VECTORS)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("vl-"))
        .collect();
    dirs.sort();
    let mut matched = 0;
    for dir in &dirs {
        let read = |f: &str| std::fs::read(Path::new(dir).join(f)).unwrap();
        let (txn, program, reply) = (read("txn.bin"), read("program.bin"), read("reply.bin"));
        let file_id = u32::from_be_bytes(txn[12..16].try_into().unwrap());
        let context = om_query::Context {
            file_id,
            connection_key: None,
        };
        let outcome = om_query::execute(&file, &program, &context);
        assert_eq!(
            outcome.slots,
            reply_slots(&reply),
            "{}: {:?}",
            dir.display(),
            outcome.error
        );
        matched += 1;
    }
    assert!(matched >= 10, "{matched} matched");
}

/// The schema instructions of the Manage Database probes
/// (`re/work/schema/live`, schema-path.md 3.4): UploadMultiple stores the
/// source spot's members under the destination and removes the delete
/// list's, DeleteTable removes the table, and each answers FileMaker
/// Server's empty reply.
#[test]
#[ignore = "requires the git-ignored schema vectors"]
fn schema_instructions_match_filemaker_server() {
    use om_query::Write;
    const LIVE: &str = "../../re/work/schema/live";
    let file = om_hbam::HbamFile::open(TEST_PROJECT).unwrap();
    let run = |name: &str| {
        let req = std::fs::read(format!("{LIVE}/{name}.req")).unwrap();
        let rep = std::fs::read(format!("{LIVE}/{name}.rep")).unwrap();
        // The op86 body: the program, u32 N, then the 19-byte resume block.
        let n_at = req.len() - 23;
        let n = u32::from_be_bytes(req[n_at..n_at + 4].try_into().unwrap()) as usize;
        let program = &req[20..20 + n];
        let file_id = u32::from_be_bytes(req[12..16].try_into().unwrap());
        let context = om_query::Context {
            file_id,
            connection_key: None,
        };
        let outcome = om_query::execute(&file, program, &context);
        assert!(outcome.error.is_none(), "{name}: {:?}", outcome.error);
        assert_eq!(outcome.slots, reply_slots(&rep), "{name}");
        outcome.writes
    };
    let paths = |writes: &[Write]| -> Vec<String> {
        writes
            .iter()
            .map(|w| {
                let (kind, path) = match w {
                    Write::Put { path, .. } => ("put", path),
                    Write::Remove { path } => ("remove", path),
                };
                let path: Vec<String> = path.iter().map(|s| hex(s)).collect();
                format!("{kind} {}", path.join("/"))
            })
            .collect()
    };
    // live:s1: a new table, its occurrence and its five fields.
    assert_eq!(paths(&run("s1/a_um16")), ["put 03/10/05/8006"]);
    assert_eq!(paths(&run("s1/a_um17")), ["put 03/11/05/d00006"]);
    let fields = run("s1/a_umf");
    assert_eq!(
        paths(&fields),
        ["01", "02", "03", "04", "05"].map(|f| format!("put 8006/03/05/{f}"))
    );
    let Write::Put { node, .. } = &fields[0] else {
        unreachable!()
    };
    assert!(
        node.child(&[0x02]).is_some(),
        "the field definition is copied"
    );
    // live:s2a: the table goes, then its catalog entries.
    assert_eq!(paths(&run("s2a/a_deltable")), ["remove 8006"]);
    assert_eq!(paths(&run("s2a/a16_um")), ["remove 03/10/05/8006"]);
    assert_eq!(paths(&run("s2a/a17_um")), ["remove 03/11/05/d00006"]);
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
