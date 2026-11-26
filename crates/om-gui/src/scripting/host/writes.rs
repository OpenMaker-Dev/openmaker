//! A script's field writes on the desktop (script-runtime.md 13.19-13.22,
//! "Implementation notes (desktop client)", Writes): Set Field, Set Field
//! By Name and the insert, paste and replace steps go through
//! [`WorkerHost::write_field`]. As on FileMaker Server, a script's write
//! follows a script's rules, not a user's:
//!
//! - the rules checked as a value is entered (maximum characters, strict
//!   data types, range, value list) are Set Field's own errors, for a field
//!   validated Always, and the field keeps its value
//!   (`DataProvider::check_entry`);
//! - the record is committed with a script's rules
//!   (`DataProvider::write_record`): any normal field and repetition,
//!   prohibit-modification auto-entry ignored, "only during data entry"
//!   rules other than the entry rules applying, Commit Records' "Skip data
//!   entry validation";
//! - a global field takes the session's value, a calculation or summary
//!   field is 201 at Set Field;
//! - a related field writes the active portal row's record, else the
//!   first related record, else creates one when the relationship allows
//!   it ([`super::portals`]); the related records are committed with the
//!   current record.

use om_data::calc::Value;
use om_data::host::FieldClass;
use om_data::runtime::{ErrorCode, FieldRef, HostError, HostResult, Mode};

use super::{data_error, WorkerHost, FIELD_LOCKED};

/// "Repetition number is invalid" (Set Field past the field's
/// repetitions, as on FileMaker Server).
const REPETITION_INVALID: i32 = 111;

/// The cell of repetition `repetition` of `field`: the field's id for the
/// first, `F * 256 + n` for the others (record-writes.md).
pub(crate) fn cell_of(field: u64, repetition: u32) -> u64 {
    if repetition <= 1 {
        field
    } else {
        field * 256 + u64::from(repetition)
    }
}

impl WorkerHost<'_> {
    pub(super) fn assign_script_field(
        &mut self,
        field: FieldRef,
        value: &Value,
        direct_set_field: bool,
    ) -> HostResult {
        self.write_field(field, value, direct_set_field)?;
        // A step's value is no longer being typed (6.3 O2).
        if let Ok(name) = self.field_name(field) {
            let state = &mut self.s.file.window.state;
            if state.typing.as_deref() == Some(name.as_str()) {
                state.typing = None;
            }
        }
        Ok(())
    }

    /// `schema`, or, when `occurrence` waits for its related file not open
    /// yet (`CalcSchema::is_pending`), the schema after that file opens: a
    /// step writing into it opens it now, as a read does (#983,
    /// `WorkerHost::field`), and writes over the schema that has it
    /// (#1149).
    pub(super) fn open_pending(
        &mut self,
        schema: &std::sync::Arc<om_data::host::CalcSchema>,
        occurrence: u64,
    ) -> HostResult<std::sync::Arc<om_data::host::CalcSchema>> {
        if !schema.is_pending(occurrence) {
            return Ok(std::sync::Arc::clone(schema));
        }
        let _ = self
            .p()
            .calc_data(&[om_data::host::CalcNeed::Occurrence { occurrence }]);
        self.reread_schema();
        self.schema().cloned().ok_or(HostError::Unsupported)
    }

    /// Write `value` into `field` as a script step does (see the module
    /// documentation).
    pub(super) fn write_field(
        &mut self,
        field: FieldRef,
        value: &Value,
        direct_set_field: bool,
    ) -> HostResult {
        let Some(mut schema) = self.schema().cloned() else {
            // Without a calculation schema only the layout's own fields
            // are known, by name.
            let name = self.field_name(field)?;
            let text = self.text(value);
            return self.set_named(&name, text);
        };
        if schema.field(field.occurrence, field.field).is_none() {
            schema = self.open_pending(&schema, field.occurrence)?;
        }
        let def = schema
            .field(field.occurrence, field.field)
            .ok_or(HostError::Error(ErrorCode::FIELD_MISSING))?
            .clone();
        let table = schema
            .table_of(field.occurrence)
            .ok_or(HostError::Error(ErrorCode::FIELD_MISSING))?
            .id;
        let repetition = field.repetition.max(1);
        if repetition > def.repetitions.max(1) {
            return Err(ErrorCode(REPETITION_INVALID).into());
        }
        // Repetition n of 2 to 255 is cell `field * 256 + n` (`cell_of`):
        // one past 255, or whose cell is another field's id in a malformed
        // file, names no cell of its own and would be written into another
        // field's, so it is not written (3).
        if repetition > 1
            && (repetition > 255
                || def.id.checked_mul(256).is_none()
                || schema
                    .table(table)
                    .is_some_and(|t| t.fields.contains_key(&cell_of(def.id, repetition))))
        {
            return Err(HostError::Unsupported);
        }
        let text = self.text(value);
        if def.global {
            // A global field holds this session's value, in Browse and
            // Find mode alike (it takes no criterion, script-runtime.md
            // 13.19): in a hosted file a client's global values are its
            // own and last until it closes the file (Claris "Global
            // fields").
            if def.value_type == om_data::calc::ValueType::Container {
                return Err(ErrorCode(FIELD_LOCKED).into());
            }
            self.p()
                .set_global(table, def.id, repetition, &text)
                .map_err(data_error)?;
            // The user's draft of that repetition gives way (#185).
            self.s
                .file
                .window
                .forget_typed_cell(table, cell_of(def.id, repetition));
            *self.s.file.cache.borrow_mut() = Default::default();
            self.s.file.window.changed_data();
            return Ok(());
        }
        if self.s.file.window.state.mode == Mode::Find {
            // A summary field takes no criterion (201, 13.19); a related
            // field's criterion is named `Occurrence::Field`.
            if def.class == FieldClass::Summary {
                return Err(ErrorCode(FIELD_LOCKED).into());
            }
            let name = self.find_field_name(field)?;
            return self.set_criterion(&name, text);
        }
        if def.class != FieldClass::Data {
            // Set Field into a calculation or summary field (13.10).
            return Err(ErrorCode(FIELD_LOCKED).into());
        }
        let related = self.is_related(field.occurrence);
        if def.value_type == om_data::calc::ValueType::Container {
            // Container data is written through the field's library, not
            // as text (containers.md).
            if related || repetition > 1 {
                return Err(HostError::Unsupported);
            }
            return self.set_container(&def.name, value.clone());
        }
        let cell = cell_of(def.id, repetition);
        if !text.is_empty() {
            self.p()
                .check_entry(table, cell, &text)
                .map_err(|e| self.failed(e))?;
        }
        if related {
            return self.set_related(field.occurrence, table, cell, text, direct_set_field);
        }
        let name = (repetition == 1).then_some(def.name.as_str());
        let (window, provider) = self.wp();
        window
            .set_cell(provider, name, cell, text)
            .map_err(data_error)
    }

    /// Write `text` into field `name` of the layout's table (the insert,
    /// paste and replace steps, Set Field By Name), as
    /// [`Self::write_field`] does.
    pub(super) fn write_named(&mut self, name: &str, text: String) -> HostResult {
        if self.s.file.window.state.mode == Mode::Find {
            return self.set_criterion(name, text);
        }
        let Some(field) = self.own_field(name) else {
            return self.set_named(name, text);
        };
        self.write_field(field, &Value::text(text), false)
    }

    /// The layout's field named `name` as a step's target.
    fn own_field(&mut self, name: &str) -> Option<FieldRef> {
        let occurrence = self.s.file.occurrence?;
        let schema = self.schema()?.clone();
        let def = schema
            .table_of(occurrence)?
            .fields
            .values()
            .find(|f| f.name == name)?;
        Some(FieldRef {
            occurrence,
            field: def.id,
            repetition: 1,
        })
    }

    /// Whether a field of `occurrence` belongs to a related record rather
    /// than the current one: another occurrence the window's reaches
    /// through relationships (a self-join included). An occurrence of the
    /// same table without a path writes the current record.
    pub(super) fn is_related(&mut self, occurrence: u64) -> bool {
        let Some(current) = self.s.file.occurrence else {
            return false;
        };
        if occurrence == current {
            return false;
        }
        let Some(schema) = self.schema() else {
            return false;
        };
        if schema.path(current, occurrence).is_some() {
            return true;
        }
        let same_table = schema
            .table_of(current)
            .zip(schema.table_of(occurrence))
            .is_some_and(|(a, b)| a.id == b.id);
        !same_table
    }

    /// A Find mode criterion of the current request.
    fn set_criterion(&mut self, name: &str, text: String) -> HostResult {
        let (window, provider) = self.wp();
        window.set(provider, name, text).map_err(data_error)
    }
}
