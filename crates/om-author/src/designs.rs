//! `compare_designs`: the comparison of two files' designs
//! (`om_hbam::design_paths::compare_files`) as a command result. The
//! result carries what the comparison reports and nothing more: kinds,
//! ids, names, classes of difference and design revisions.

use std::path::Path;

use om_hbam::design_paths::{
    compare_files, Alone, ChangedBy, CompareError, Comparison, DesignRevision, Difference, Ident,
    IdentityProblem, Matched, Presence, ReadFailure, DESIGN_REVISION_VERSION,
};
use serde_json::{json, Map, Value};

use crate::api::{AuthorError, CompareDesignsParams, ErrorKind};

/// Run `compare_designs`.
pub fn compare_designs(params: &CompareDesignsParams) -> Result<Value, AuthorError> {
    let comparison = compare_files(
        Path::new(&params.source),
        Path::new(&params.target),
        params.base.as_deref().map(Path::new),
    )
    .map_err(error)?;
    Ok(result(&comparison, params.include_same))
}

/// The command's error: the comparison's typed error, which names a file,
/// a kind, a table and an id at most (the id only when the key has its
/// specified shape), never a key's bytes.
fn error(error: CompareError) -> AuthorError {
    let (kind, details) = match error {
        CompareError::Read { which, failure } => {
            let (kind, reason) = match failure {
                ReadFailure::Missing => (ErrorKind::NotFound, "missing"),
                ReadFailure::PermissionDenied => (ErrorKind::PermissionDenied, "permission_denied"),
                ReadFailure::Io => (ErrorKind::InvalidRequest, "io"),
                ReadFailure::NotFileMaker => (ErrorKind::InvalidRequest, "not_filemaker"),
                ReadFailure::Damaged => (ErrorKind::InvalidRequest, "damaged"),
            };
            (kind, json!({"file": which.name(), "reason": reason}))
        }
        CompareError::TooDeep { which } => (
            ErrorKind::InvalidRequest,
            json!({"file": which.name(), "reason": "too_deep"}),
        ),
        CompareError::Identity { which, error } => {
            let reason = match error.problem {
                IdentityProblem::Unsorted => "unsorted_keys",
                IdentityProblem::KeyShape => "key_shape",
                IdentityProblem::Duplicate => "duplicate_id",
                IdentityProblem::Misread => "misread",
            };
            (
                ErrorKind::InvalidRequest,
                json!({"file": which.name(), "reason": reason,
                       "kind": error.kind.map(|k| k.name()), "table": error.table,
                       "id": error.id}),
            )
        }
    };
    AuthorError::new(kind, error.to_string()).with_details(details)
}

fn revision(revision: &Option<DesignRevision>) -> Value {
    json!(revision.as_ref().map(DesignRevision::text))
}

fn ident(object: &Ident) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("kind".into(), json!(object.kind.name()));
    out.insert("catalog".into(), json!(object.catalog));
    if let Some(table) = object.table {
        out.insert("table".into(), json!(table));
    }
    out.insert("id".into(), json!(object.id));
    out.insert("name".into(), json!(object.name));
    out
}

fn changed_by(by: Option<ChangedBy>) -> Value {
    json!(by.map(|by| match by {
        ChangedBy::Source => "source",
        ChangedBy::Target => "target",
        ChangedBy::Both => "both",
    }))
}

fn matched(m: &Matched) -> Value {
    let mut out = ident(&m.source);
    out.insert("target_name".into(), json!(m.target_name));
    out.insert("difference".into(), json!(m.difference.parts()));
    out.insert("source_revision".into(), revision(&m.source_revision));
    out.insert("target_revision".into(), revision(&m.target_revision));
    if let Some(base) = &m.base {
        out.insert("base_revision".into(), revision(&base.revision));
        out.insert("source_change".into(), json!(base.source.parts()));
        out.insert("target_change".into(), json!(base.target.parts()));
        out.insert("changed_by".into(), changed_by(base.changed_by()));
        out.insert("conflict".into(), json!(m.conflict()));
    }
    Value::Object(out)
}

fn alone(a: &Alone) -> Value {
    let mut out = ident(&a.object);
    out.insert("revision".into(), revision(&a.revision));
    let (presence, extra) = match a.presence {
        Presence::Only => ("only", None),
        Presence::New { id_taken } => ("new", Some(("id_taken", id_taken))),
        Presence::DeletedInOther { changed } => ("deleted_in_other", Some(("changed", changed))),
    };
    out.insert("presence".into(), json!(presence));
    if let Some((key, value)) = extra {
        out.insert(key.into(), json!(value));
    }
    Value::Object(out)
}

/// The result: counts of the matched objects that are the same and of
/// those in which each part differs, then the objects.
fn result(comparison: &Comparison, include_same: bool) -> Value {
    let mut counts: std::collections::BTreeMap<&str, usize> = Difference::PARTS
        .iter()
        .chain(&["same"])
        .map(|name| (*name, 0))
        .collect();
    for m in &comparison.matched {
        if m.difference.is_same() {
            *counts.entry("same").or_default() += 1;
        }
        for part in m.difference.parts() {
            *counts.entry(part).or_default() += 1;
        }
    }
    let without_revision = comparison
        .matched
        .iter()
        .filter(|m| m.source_revision.is_none() || m.target_revision.is_none())
        .count();
    let listed: Vec<Value> = comparison
        .matched
        .iter()
        .filter(|m| include_same || !m.difference.is_same())
        .map(matched)
        .collect();
    let possible: Vec<Value> = comparison
        .possible_matches
        .iter()
        .map(|p| {
            json!({
                "source": Value::Object(ident(&p.source)),
                "target": Value::Object(ident(&p.target)),
                "agrees": match p.agrees {
                    om_hbam::design_paths::Agreement::Id => "id",
                    om_hbam::design_paths::Agreement::Name => "name",
                },
                "source_revision": revision(&p.source_revision),
                "target_revision": revision(&p.target_revision),
            })
        })
        .collect();
    json!({
        "with_base": comparison.with_base,
        "design_revision_version": DESIGN_REVISION_VERSION,
        "counts": {
            "matched": comparison.matched.len(),
            "matched_without_design_revision": without_revision,
            "differences": counts,
            "conflicts": comparison.matched.iter().filter(|m| m.conflict()).count(),
            "only_in_source": comparison.only_in_source.len(),
            "only_in_target": comparison.only_in_target.len(),
            "possible_matches": comparison.possible_matches.len(),
        },
        "matched": listed,
        "only_in_source": comparison.only_in_source.iter().map(alone).collect::<Vec<_>>(),
        "only_in_target": comparison.only_in_target.iter().map(alone).collect::<Vec<_>>(),
        "possible_matches": possible,
        "possible_matches_left_out": comparison.possible_matches_left_out,
    })
}
