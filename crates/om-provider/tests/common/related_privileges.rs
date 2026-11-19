//! Synthetic owning-file privilege formulas shared with headless script tests.
#![allow(dead_code)]

use om_formula::NameTable;
use om_hbam::OwnedNode;
use om_scripts::{Calculation, Script};

use crate::{fixture, related, security};

pub const ROOT: &str = "Root.fmp12";
pub const RELATED: &str = "Related.fmp12";
pub const NESTED: &str = "Nested.fmp12";
pub const UNKNOWN: &str = "Unknown.fmp12";
pub const REMOTE: u64 = 0xd0_0040;
pub const DEEP: u64 = 0xd0_0041;
pub const UNAVAILABLE: u64 = 0xd0_0042;
pub const REPORT_FIELD: u64 = 90;
pub const NESTED_FIELD: u64 = 91;
pub const DEEP_FIELD: u64 = 92;

pub const REPORT: &str = "Get ( CurrentPrivilegeSetName ) & \"|\" & Get ( AccountPrivilegeSetName ) & \"|\" & Substitute ( Get ( CurrentExtendedPrivileges ) ; Char ( 13 ) ; \",\" ) & \"|\" & Substitute ( Get ( AccountExtendedPrivileges ) ; Char ( 13 ) ; \",\" ) & \"|\" & Get ( LayoutAccess ) & \"|\" & Get ( RecordAccess )";
pub const ROOT_ANSWER: &str = "Office|Office|zeta,alpha|zeta,alpha||2";
pub const ELEVATED: &str = "[Full Access]|Office|fmapp,full-only|zeta,alpha|2|2";
pub const RELATED_ANSWER: &str = "[Read-Only Access]|[Read-Only Access]|fmapp|fmapp||1";
pub const NESTED_ANSWER: &str = "[Data Entry Only]|[Data Entry Only]|fmapp|fmapp||2";
/// Metadata the provider could not read.
pub const UNKNOWN_ANSWER: &str = "|||||";
/// Unknown.fmp12, the plain Tasks file: Admin in [Full Access], with no
/// extended privileges.
pub const PLAIN_ANSWER: &str = "[Full Access]|[Full Access]||||2";

pub fn names() -> NameTable {
    let mut names = fixture::names();
    for (id, name) in [
        (REMOTE, "Remote Tasks"),
        (DEEP, "Deep Tasks"),
        (UNAVAILABLE, "Unknown Tasks"),
    ] {
        names.add_occurrence(id, name, fixture::TABLE);
    }
    for (id, name) in [
        (REPORT_FIELD, "Privileges"),
        (NESTED_FIELD, "NestedPrivileges"),
        (DEEP_FIELD, "DeepPrivileges"),
    ] {
        names.add_field(fixture::TABLE, id, name);
    }
    names
}

pub fn calculation(text: &str) -> Calculation {
    let names = names();
    let mut ctx = om_formula::Context::new(&names);
    ctx.occurrence = Some(fixture::OCCURRENCE);
    let compiled = om_formula::compile(text, &ctx).unwrap();
    Calculation::new(
        Some(om_formula::formatted::encode(&compiled.tokens)),
        om_formula::polish::encode(&compiled.expr),
    )
}

fn field(tree: &mut OwnedNode, id: u64, name: &str, expression: &str) {
    fixture::add_field(tree, fixture::TABLE, id, name, 2, 1, 0, 0x2_0020, 0);
    fixture::add_formula(
        tree,
        &names(),
        (fixture::TABLE, fixture::OCCURRENCE),
        id,
        5,
        expression,
    );
}

pub fn files(scripts: &[Script]) -> Vec<(&'static str, OwnedNode)> {
    let mut root = security::tree(scripts, security::CUSTOM_KEY);
    let mut child = security::tree(&[], &[3]);
    let mut nested = security::tree(&[], &[2]);
    let mut unknown = fixture::tree(&[], true);
    // The related login returns a different, opaque account key from root.
    let account = child.remove(&[&[23], &[1], &[5], &[2]]).unwrap();
    child.put(&[&[23], &[1], &[5], &[0x80, 0x81]], account);
    for tree in [&mut root, &mut child, &mut nested, &mut unknown] {
        field(tree, REPORT_FIELD, "Privileges", REPORT);
        field(
            tree,
            NESTED_FIELD,
            "NestedPrivileges",
            "Evaluate ( \"Privileges\" )",
        );
    }
    for (tree, occurrence, name, source, file) in [
        (&mut root, REMOTE, "Remote Tasks", 1, "Related"),
        (&mut child, DEEP, "Deep Tasks", 1, "Nested"),
    ] {
        related::reference(tree, (occurrence, name), source, file);
        fixture::add_relationship(
            tree,
            9,
            (fixture::OCCURRENCE, fixture::NAME),
            0,
            (occurrence, fixture::NAME),
        );
    }
    related::reference(&mut root, (UNAVAILABLE, "Unknown Tasks"), 2, "Unknown");
    fixture::add_relationship(
        &mut root,
        10,
        (fixture::OCCURRENCE, fixture::NAME),
        0,
        (UNAVAILABLE, fixture::NAME),
    );
    field(
        &mut child,
        DEEP_FIELD,
        "DeepPrivileges",
        "Deep Tasks::NestedPrivileges",
    );
    vec![
        (ROOT, root),
        (RELATED, child),
        (NESTED, nested),
        (UNKNOWN, unknown),
    ]
}
