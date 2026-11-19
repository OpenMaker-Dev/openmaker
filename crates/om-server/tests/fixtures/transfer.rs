//! Import Records and Export Records steps as FileMaker stores them, for
//! the server script tests of `fixtures/tasks.rs` files.
#![allow(dead_code)]

use om_hbam::OwnedNode;
use om_scripts::{flag, ScriptStep};
use om_transfer::spec::{
    ExportField, ExportOptions, FieldSpec, ImportAction, ImportOptions, Key, MapEntry, Mapping,
    ProfileEx,
};
use om_transfer::{ExportCharset, FileType, ImportCharset, Profile};

use super::fixture::OCCURRENCE;

/// A path list as FileMaker stores it (script-steps.md 11.0): each path's
/// kind (`file`, `filemac`, ...) and path.
pub fn path_stream(paths: &[(&str, &str)]) -> Vec<u8> {
    let masked = |s: &str| -> Vec<u8> {
        let mut out = vec![s.len() as u8];
        out.extend(s.bytes().map(|b| b ^ 0x5a));
        out
    };
    let mut out = (paths.len() as u32).to_be_bytes().to_vec();
    for (kind, p) in paths {
        out.extend(masked(kind));
        out.extend(masked(""));
        out.extend(masked(p));
    }
    out
}

pub fn with_paths(step: ScriptStep, paths: &[&str]) -> ScriptStep {
    let paths: Vec<(&str, &str)> = paths.iter().map(|p| ("file", *p)).collect();
    with_kinds(step, &paths)
}

pub fn with_kinds(mut step: ScriptStep, paths: &[(&str, &str)]) -> ScriptStep {
    step.set_slot_child(0, 0x82, Some(OwnedNode::leaf([0x82], path_stream(paths))));
    step.flags |= flag::FILE_PATH;
    step
}

/// Export Records [No dialog; tab-separated UTF-8; Tasks fields].
pub fn export(paths: &[&str], fields: &[u64], file_type: FileType) -> ScriptStep {
    let options = ExportOptions {
        fields: fields
            .iter()
            .map(|f| ExportField {
                field: FieldSpec::new(OCCURRENCE, *f),
                sum_by: FieldSpec::none(),
            })
            .collect(),
        group_by: Vec::new(),
        formatting: false,
        charset: ExportCharset::Utf8,
        use_name_alias: Some(1),
    };
    export_with(paths, options, file_type)
}

/// Export Records [No dialog; `options`].
pub fn export_with(paths: &[&str], options: ExportOptions, file_type: FileType) -> ScriptStep {
    let mut step = ScriptStep::new(36);
    step.set_data_leaf(0x85, Some(OwnedNode::leaf([0x85], options.encode())));
    step.set_data_leaf(
        0x86,
        Some(OwnedNode::leaf([0x86], Profile::new(file_type).encode())),
    );
    step.flags |= flag::NO_INTERACT | flag::RESTORE | flag::EXTENDED_DATA;
    with_paths(step, paths)
}

/// Import Records [No dialog; tab-separated UTF-8; add into Tasks].
pub fn import(paths: &[&str], fields: &[u64], auto_enter: bool) -> ScriptStep {
    import_as(paths, fields, auto_enter, ImportAction::Add)
}

/// Import Records [No dialog; tab-separated UTF-8; `action` into Tasks].
pub fn import_as(
    paths: &[&str],
    fields: &[u64],
    auto_enter: bool,
    action: ImportAction,
) -> ScriptStep {
    import_into(OCCURRENCE, paths, fields, auto_enter, action)
}

/// Import Records [No dialog; tab-separated UTF-8; `action` into the table
/// occurrence `target`].
pub fn import_into(
    target: u64,
    paths: &[&str],
    fields: &[u64],
    auto_enter: bool,
    action: ImportAction,
) -> ScriptStep {
    let options = ImportOptions {
        table: Key::from_id(target),
        action,
        flags: if auto_enter { 0x02 } else { 0 },
        charset: ImportCharset::Utf8,
        map: fields
            .iter()
            .map(|f| MapEntry {
                field: Key::from_id(*f),
                mapping: Mapping::Import,
            })
            .collect(),
    };
    let ex = ProfileEx {
        field_names_row: -1,
        predefined: -1,
        delimiter: "\t".into(),
    };
    let mut step = ScriptStep::new(35);
    step.value = 1;
    step.set_data_leaf(0x84, Some(OwnedNode::leaf([0x84], options.encode())));
    step.set_data_leaf(
        0x86,
        Some(OwnedNode::leaf(
            [0x86],
            Profile::new(FileType::TabSeparated).encode(),
        )),
    );
    step.set_data_leaf(0x87, Some(OwnedNode::leaf([0x87], ex.encode())));
    step.flags |= flag::NO_INTERACT | flag::RESTORE | flag::EXTENDED_DATA | flag::MENU_SELECTION;
    with_paths(step, paths)
}
