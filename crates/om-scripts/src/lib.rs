//! Native FileMaker script definitions: the script catalog codec, typed step
//! parameters, an edit API that keeps cached block indexes right, the
//! per-step compatibility table, the script runtime ([`runtime`]) that
//! executes them against a [`runtime::ScriptHost`], and the commands that
//! are one step ([`save_records`], [`revert_record`]).
//!
//! Decoding keeps every stored byte (descriptor filler, stale inline
//! references, unknown leaves and flag bits, unreferenced step data), so a
//! script re-encodes exactly. Specs: docs/re/scripts.md, script-steps.md,
//! script-runtime.md and their catalogs.

use std::fmt;

mod catalog;
pub mod display;
mod flow;
mod key;
mod params;
pub mod paths;
pub mod revert_record;
pub mod runtime;
pub mod save_records;
mod schema;
mod script;
pub mod specs;
mod step;
mod value;

pub use catalog::{
    is_server_step_id, is_supported, is_supported_on_server, is_valid_step_id, step_compat,
    step_ids, step_info, step_name, supported_clients, ClarisSupport, Client, StepCompat, StepInfo,
    Support, NOT_BUTTON_COMPATIBLE, OBSOLETE_STEP_IDS,
};
pub use flow::{analyze as analyze_blocks, caches_index, Cached};
pub use key::{FieldSpec, StoredKey};
pub use params::{Item, ParamValue, Parameter, Unclassified};
pub use runtime::{Options, RunState, Runtime, ScriptCall, ScriptHost};
pub use schema::{
    step_schema, step_schemas, Location, ParamKind, ParamSchema, RefKind, SlotKind, Source,
    StepSchema,
};
pub use script::{Script, ScriptCatalog, ScriptKind, StepSource};
pub use step::{flag, Calculation, ScriptStep, Slot, StepData, VARIABLE_NAME_SLOT};
pub use value::{bytes_node, encoded_text_node, node_bytes, node_text, text_node};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptError(pub String);
impl fmt::Display for ScriptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for ScriptError {}
type Result<T> = std::result::Result<T, ScriptError>;

/// A local catalog reference (`HBKey<5>`: length, one segment) read as its
/// numeric id. Only `1 + length` bytes are read; the filler after the key
/// varies and is not part of it (script-steps.md 2.1).
pub fn reference_id(bytes: &[u8]) -> Option<u64> {
    let len = usize::from(*bytes.first()?);
    let (&seglen, segment) = bytes.get(1..1 + len)?.split_first()?;
    if usize::from(seglen) != segment.len() || segment.is_empty() {
        return None;
    }
    om_hbam::slot::be_uint(segment)
}
