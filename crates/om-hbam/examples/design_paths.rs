//! Read-only report of how a local file's paths fall into the path groups
//! of `om_hbam::design_paths` (`docs/server-storage.md` section 4): the
//! terminal paths per group, the paths no rule covers by shape, and the
//! catalog members that are no design object. With `--objects`, also each
//! design object with its kind, key, name and revision. Several files give
//! one report over all of them.
//!
//! `cargo run -p om-hbam --example design_paths -- [--objects] <file.fmp12>...`
use om_hbam::design_paths::{self, hex, revision_text, Report};
use om_hbam::HbamFile;

fn main() {
    let mut objects = false;
    let mut files = Vec::new();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--objects" => objects = true,
            _ => files.push(arg),
        }
    }
    if files.is_empty() {
        eprintln!("usage: design_paths [--objects] <file.fmp12>...");
        std::process::exit(2);
    }
    let mut total = Report::default();
    for path in &files {
        let file = HbamFile::open(path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let root = file
            .key_tree()
            .unwrap_or_else(|e| panic!("{path}: {e}"))
            .root
            .to_owned_node();
        let report = design_paths::report(&root);
        for (group, count) in report.groups {
            *total.groups.entry(group).or_default() += count;
        }
        total.structure += report.structure;
        for (shape, count) in report.unclassified {
            *total.unclassified.entry(shape).or_default() += count;
        }
        total.members.extend(report.members);
        if objects {
            println!("{path}:");
            for object in design_paths::objects(&root) {
                let table = object
                    .table
                    .as_ref()
                    .map(|t| format!("{}.", hex(t)))
                    .unwrap_or_default();
                println!(
                    "  {:<18} {table}{:<8} {:<40} {}",
                    object.kind.name(),
                    hex(&object.key),
                    object.name.as_deref().unwrap_or("-"),
                    revision_text(&object.revision())
                );
            }
        }
    }
    print!("{total}");
}
