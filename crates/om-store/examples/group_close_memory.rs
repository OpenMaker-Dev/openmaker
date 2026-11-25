//! What a staged group's close holds in memory for its edits
//! (`docs/server-storage.md` section 6.9), apart from the page file: the
//! staged bytes, decoded as the close decodes them, and applied to an
//! empty tree in one unit of work over an in-memory page store, as the
//! close applies them to a file's tree. It prints the process's peak
//! memory before, after decoding, and after the apply, for one shape of
//! edits: `cargo run --release -p om-store --example group_close_memory --
//! <edits> <value bytes> [empty segments]`, each edit a put at
//! `[g, <empty segments>, i]`. A shape at the bounds the store holds a staged
//! group to (`apply::MAX_GROUP_EDITS` edits, `apply::MAX_GROUP_STAGED`
//! bytes as they encode) is refused here when it passes them.

use om_store::apply::{MAX_GROUP_EDITS, MAX_GROUP_STAGED};
use om_store::entry::{decode_edits, encode_edits};
use om_store::page::{EntryPages, MemStore, Numbering, PageStore, TreeName};
use om_store::tree::{self, Edit};

/// The process's peak resident memory, in MiB (`VmHWM`).
fn peak_mib() -> f64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<f64>().ok())
        .unwrap_or(0.0)
        / 1024.0
}

fn main() {
    let mut args = std::env::args().skip(1);
    let edits: u32 = args.next().and_then(|a| a.parse().ok()).expect("edits");
    let value: usize = args
        .next()
        .and_then(|a| a.parse().ok())
        .expect("value bytes");
    let depth: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(0);
    assert!(
        u64::from(edits) <= MAX_GROUP_EDITS,
        "past the edits a group may stage"
    );
    let start = peak_mib();
    // The staged bytes, made a part at a time (a part's edits are encoded
    // and dropped), as the staging tree would hold them.
    let mut staged = Vec::new();
    let per_part = 1024u32;
    let mut i = 0u32;
    while i < edits {
        let n = per_part.min(edits - i);
        let part: Vec<Edit> = (i..i + n)
            .map(|k| {
                let mut v = vec![0u8; value];
                v.iter_mut()
                    .enumerate()
                    .for_each(|(j, b)| *b = (k as usize + j) as u8);
                let mut path = vec![b"g".to_vec()];
                path.extend(std::iter::repeat_n(Vec::new(), depth));
                path.push(k.to_be_bytes().to_vec());
                Edit::leaf(path, v)
            })
            .collect();
        staged.push(encode_edits(&part).expect("encode"));
        i += n;
    }
    let bytes: u64 = staged.iter().map(|p| p.len() as u64).sum();
    assert!(
        bytes <= MAX_GROUP_STAGED,
        "{bytes} bytes, past the bytes a group may stage"
    );
    let staged_peak = peak_mib();
    // The close: every part decoded into one list, then one apply.
    let mut all: Vec<Edit> = Vec::new();
    for part in &staged {
        all.extend(decode_edits(part).expect("decode"));
    }
    drop(staged);
    let decoded = peak_mib();
    let store = MemStore::new(Numbering::Sequential);
    let mut unit = store.begin().expect("unit");
    let root = tree::apply(&store, &mut unit, None, 1, &all).expect("apply");
    unit.add_tree(&TreeName::new(b"file:1".to_vec()).expect("name"), root);
    unit.commit().expect("commit");
    let applied = peak_mib();
    println!(
        "{edits} edits of {value} B at depth {} ({bytes} B encoded): peak {start:.1} MiB at the start, \
         {staged_peak:.1} with the staged bytes, {decoded:.1} decoded, {applied:.1} applied; \
         {} pages in the tree",
        depth + 2,
        store.len()
    );
}
