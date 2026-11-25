//! File-qualified references: `kind:id@file`, such as
//! `script:3@Tasks.fmp12`, `field:32769.2@Tasks.fmp12` (a table's field) or
//! `layout_object:1.4@Tasks.fmp12` (a layout's object). Ids are the
//! file's own (a table's serial key value, a script's id), so a reference
//! means nothing without its file: one naming another file than the one
//! connected to is refused, never resolved to whatever has the same id
//! here.

use std::fmt::Display;

use om_data::file_refs::hosted_key;

use crate::api::{AuthorError, ErrorKind};

/// The reference to object `id` of `kind` in `file`.
pub fn reference(kind: &str, id: impl Display, file: &str) -> String {
    format!("{kind}:{id}@{file}")
}

/// A reference's kind, id and file.
fn parse(reference: &str) -> Option<(&str, &str, &str)> {
    let (kind, rest) = reference.split_once(':')?;
    let (id, file) = rest.split_once('@')?;
    Some((kind, id, file))
}

/// The script id `reference` names in `file`, the file connected to.
pub fn script_id(reference: &str, file: &str) -> Result<u64, AuthorError> {
    id_of("script", "script:3", reference, file)
}

/// The table id `reference` names in `file`, the file connected to.
pub fn table_id(reference: &str, file: &str) -> Result<u64, AuthorError> {
    id_of("table", "table:32769", reference, file)
}

/// The table occurrence id `reference` names in `file`, the file
/// connected to.
pub fn occurrence_id(reference: &str, file: &str) -> Result<u64, AuthorError> {
    id_of("occurrence", "occurrence:13631490", reference, file)
}

/// The layout id `reference` names in `file`, the file connected to.
pub fn layout_id(reference: &str, file: &str) -> Result<u64, AuthorError> {
    id_of("layout", "layout:1", reference, file)
}

/// The value list id `reference` names in `file`, the file connected to.
pub fn value_list_id(reference: &str, file: &str) -> Result<u64, AuthorError> {
    id_of("value_list", "value_list:3", reference, file)
}

/// The custom function id `reference` (`custom_function:1@Tasks.fmp12`)
/// names in `file`, the file connected to.
pub fn custom_function_id(reference: &str, file: &str) -> Result<u64, AuthorError> {
    id_of("custom_function", "custom_function:1", reference, file)
}

/// The data source id `reference` (`data_source:1@Tasks.fmp12`, as
/// `inspect` gives it) names in `file`, the file connected to.
pub fn data_source_id(reference: &str, file: &str) -> Result<u64, AuthorError> {
    id_of("data_source", "data_source:1", reference, file)
}

/// The relationship id `reference` (`relationship:1@Tasks.fmp12`) names in
/// `file`, the file connected to.
pub fn relationship_id(reference: &str, file: &str) -> Result<u64, AuthorError> {
    id_of("relationship", "relationship:1", reference, file)
}

/// The table and field ids `reference` names in `file`, the file
/// connected to.
pub fn field_id(reference: &str, file: &str) -> Result<(u64, u64), AuthorError> {
    let id = text_of("field", "field:32769.2", reference, file)?;
    id.split_once('.')
        .and_then(|(table, field)| Some((table.parse().ok()?, field.parse().ok()?)))
        .ok_or_else(|| {
            AuthorError::new(
                ErrorKind::InvalidRequest,
                format!("“{reference}” has no table and field id, as field:32769.2@{file} has."),
            )
        })
}

/// The layout and object key `reference` (`layout_object:1.4@File.fmp12`)
/// names in `file`, the file connected to.
pub fn layout_object_id(reference: &str, file: &str) -> Result<(u64, u64), AuthorError> {
    let id = text_of("layout_object", "layout_object:1.4", reference, file)?;
    id.split_once('.')
        .and_then(|(layout, object)| Some((layout.parse().ok()?, object.parse().ok()?)))
        .ok_or_else(|| {
            AuthorError::new(
                ErrorKind::InvalidRequest,
                format!(
                    "“{reference}” has no layout and object key, as layout_object:1.4@{file} has."
                ),
            )
        })
}

/// The id of the `wanted` object `reference` names in `file`; `example`
/// shows such a reference without its file.
fn id_of(wanted: &str, example: &str, reference: &str, file: &str) -> Result<u64, AuthorError> {
    text_of(wanted, example, reference, file)?
        .parse()
        .map_err(|_| {
            AuthorError::new(
                ErrorKind::InvalidRequest,
                format!("“{reference}” has no {wanted} id."),
            )
        })
}

/// The id text of the `wanted` object `reference` names in `file`.
fn text_of<'r>(
    wanted: &str,
    example: &str,
    reference: &'r str,
    file: &str,
) -> Result<&'r str, AuthorError> {
    let invalid = |message: String| AuthorError::new(ErrorKind::InvalidRequest, message);
    let (kind, id, named) = parse(reference).ok_or_else(|| {
        invalid(format!(
            "“{reference}” is not a reference such as {example}@{file}."
        ))
    })?;
    if kind != wanted {
        return Err(invalid(format!(
            "“{reference}” names a {kind}, not a {wanted}."
        )));
    }
    if hosted_key(named) != hosted_key(file) {
        return Err(AuthorError::new(
            ErrorKind::WrongFile,
            format!(
                "“{reference}” is a {wanted} of {named}, but this connection has {file} open; a \
                 {wanted} with the same id here would be another {wanted}."
            ),
        ));
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references_name_their_file() {
        assert_eq!(
            reference("script", 3, "Tasks.fmp12"),
            "script:3@Tasks.fmp12"
        );
        assert_eq!(script_id("script:3@tasks", "Tasks.fmp12").unwrap(), 3);
        // A file name may hold '@'.
        assert_eq!(script_id("script:7@a@b.fmp12", "a@b.fmp12").unwrap(), 7);
        assert_eq!(
            script_id("script:3@Other.fmp12", "Tasks.fmp12")
                .unwrap_err()
                .kind,
            ErrorKind::WrongFile
        );
        assert_eq!(
            table_id("table:32769@Tasks.fmp12", "Tasks.fmp12").unwrap(),
            32769
        );
        assert_eq!(
            table_id("table:32769@Other.fmp12", "Tasks.fmp12")
                .unwrap_err()
                .kind,
            ErrorKind::WrongFile
        );
        assert_eq!(layout_id("layout:4@Tasks.fmp12", "Tasks.fmp12").unwrap(), 4);
        assert_eq!(
            field_id("field:32769.128@Tasks.fmp12", "Tasks.fmp12").unwrap(),
            (32769, 128)
        );
        for bad in ["field:32769@Tasks.fmp12", "table:1@Tasks.fmp12"] {
            assert_eq!(
                field_id(bad, "Tasks.fmp12").unwrap_err().kind,
                ErrorKind::InvalidRequest
            );
        }
        assert_eq!(
            field_id("field:1.2@Other.fmp12", "Tasks.fmp12")
                .unwrap_err()
                .kind,
            ErrorKind::WrongFile
        );
        for bad in ["3", "layout:3@Tasks.fmp12", "script:x@Tasks.fmp12"] {
            assert_eq!(
                script_id(bad, "Tasks.fmp12").unwrap_err().kind,
                ErrorKind::InvalidRequest
            );
        }
    }
}
