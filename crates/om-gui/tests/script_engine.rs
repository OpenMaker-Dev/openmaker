//! Scripts running on the provider worker, driven through the window: Show
//! Custom Dialog with scripted answers, error alerts that continue or
//! cancel, a trigger that cancels a commit, a find and sort script with
//! stored specs, window steps around a pause, Esc stopping a script,
//! ExecuteSQL over the provider's calculation data, transactions that
//! commit, revert and fail (also from sub-scripts, and around
//! OnRecordCommit), data files, Insert from Index, Set Next Serial
//! Value, a sort by value list and wildcard finds, the order of
//! OnLayoutEnter and OnRecordLoad, summary fields over the window's found
//! set, and the menu and toolbar commands that save records as PDF and
//! Excel.
mod common;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use egui_kittest::kittest::Queryable;
use om_data::calc::{Ctx, Date, Locale, ValueType};
use om_data::formula::{compile, formatted, polish, Context, NameTable};
use om_data::found::{FoundSet, FoundSetChange, MemoryFoundSet};
use om_data::host::{
    stored_value, CalcData, CalcNeed, CalcSchema, FieldClass, FieldDef, LayoutEntry, MemorySource,
    RecordValues, Recorder, SummaryDef, SummaryKind,
};
use om_data::meta::FileOptions;
use om_data::runtime::{TransactionOptions, TriggerEvent};
use om_data::specs::{encode_requests, SortEntry, SortSpec, SpecField, StoredRequest};
use om_data::{
    flag, Calculation, Connection, DataError, DataProvider, Field, FieldChanges, FieldKind,
    FieldSpec, HostedFile, LayoutInfo, Record, RecordPage, RenderLayout, Script, ScriptCatalog,
    ScriptStep, StoredKey,
};
use om_layout::{FieldObject, Layout, LayoutDef, LayoutObject, ObjectStore, Part, Rect};

/// The layouts' table occurrence: alias 5 is occurrence 5.
const OCCURRENCE: u64 = 5;
const FIELDS: [(&str, FieldKind); 3] = [
    ("Name", FieldKind::Text),
    ("Age", FieldKind::Number),
    ("Note", FieldKind::Text),
];
const ENTRY: u32 = 1;
const OTHER: u32 = 2;
/// Summary fields of the calculation schema, after the fields: name, kind
/// (`SummaryKind` code), flags (`0x04` running) and summarized field (1
/// Name, 2 Age).
const SUMMARIES: [(&str, u8, u8, u64); 7] = [
    ("Total Age", 1, 0, 2),
    ("Running Age", 1, 0x04, 2),
    ("Average Age", 2, 0, 2),
    ("Names", 9, 0, 1),
    ("Count Names", 3, 0, 1),
    ("Oldest", 5, 0, 2),
    ("Age Share", 7, 0, 2),
];

#[derive(Default)]
struct State {
    rows: Vec<Record>,
    found: MemoryFoundSet,
    events: Vec<String>,
    scripts: ScriptCatalog,
    /// Layout triggers of the Entry layout.
    triggers: Vec<om_layout::ScriptTrigger>,
    /// The provider describes the file for calculations.
    calc_schema: bool,
    import_supported: bool,
    /// An open transaction's working copy of the records.
    working: Option<Vec<Record>>,
    /// The file's document preferences in that description (implies
    /// `calc_schema`).
    file_options: Option<FileOptions>,
    /// The next record update panics, as a provider defect would.
    panic_on_update: bool,
    /// So many of the next reads of unsettled saves panic (each script's
    /// end reads them).
    panics_on_unsettled: u32,
}

impl State {
    /// The records a page shows: the transaction's, while one is open.
    fn rows_mut(&mut self) -> &mut Vec<Record> {
        match &mut self.working {
            Some(rows) => rows,
            None => &mut self.rows,
        }
    }
}

struct Provider(Arc<Mutex<State>>);

fn fields() -> Vec<Field> {
    FIELDS
        .iter()
        .map(|(name, kind)| Field {
            name: (*name).into(),
            kind: *kind,
        })
        .collect()
}

impl DataProvider for Provider {
    fn supports_record_import(&self) -> bool {
        self.0.lock().unwrap().import_supported
    }

    fn can_write_records(&self) -> bool {
        true
    }
    fn open(&mut self, _: &Connection) -> Result<(), DataError> {
        Ok(())
    }
    fn list_files(&mut self, _: &str, _: u16) -> Result<Vec<HostedFile>, DataError> {
        Ok(vec![])
    }
    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        Ok(vec![
            LayoutInfo {
                id: ENTRY,
                name: "Entry".into(),
            },
            LayoutInfo {
                id: OTHER,
                name: "Other".into(),
            },
        ])
    }
    fn scripts(&mut self) -> Result<ScriptCatalog, DataError> {
        Ok(self.0.lock().unwrap().scripts.clone())
    }
    fn records(&mut self, _: u32, offset: u64, limit: u32) -> Result<RecordPage, DataError> {
        let state = self.0.lock().unwrap();
        let rows = state.working.as_ref().unwrap_or(&state.rows);
        Ok(state.found.page(fields(), rows, offset, limit))
    }
    fn change_found_set(&mut self, _: u32, change: FoundSetChange) -> Result<FoundSet, DataError> {
        let mut state = self.0.lock().unwrap();
        let State {
            rows,
            found,
            working,
            ..
        } = &mut *state;
        found.apply(&fields(), working.as_ref().unwrap_or(rows), change)
    }
    fn begin_transaction(&mut self, _: TransactionOptions) -> Result<(), DataError> {
        let mut state = self.0.lock().unwrap();
        state.working = Some(state.rows.clone());
        state.events.push("begin".into());
        Ok(())
    }
    fn commit_transaction(&mut self, _: bool) -> Result<(), DataError> {
        let mut state = self.0.lock().unwrap();
        let working = state.working.take().unwrap_or_default();
        if working
            .iter()
            .any(|r| r.values[0].as_deref() == Some("invalid"))
        {
            // A validation calculation refuses it: nothing is written.
            state.events.push("refused".into());
            return Err(DataError::with_code(507, "Name failed its validation."));
        }
        state.rows = working;
        state.events.push("commit".into());
        Ok(())
    }
    fn revert_transaction(&mut self) -> Result<(), DataError> {
        let mut state = self.0.lock().unwrap();
        if state.working.take().is_some() {
            state.events.push("revert".into());
        }
        Ok(())
    }
    fn value_list(&mut self, id: u64) -> Result<Vec<String>, DataError> {
        assert_eq!(id, 7);
        Ok(vec!["Edsger".into(), "-".into(), "Ada".into()])
    }
    fn index_values(&mut self, _: u32, field: &str) -> Result<Vec<String>, DataError> {
        let state = self.0.lock().unwrap();
        let i = FIELDS.iter().position(|(n, _)| *n == field).unwrap();
        let mut values: Vec<String> = state
            .rows
            .iter()
            .filter_map(|r| r.values[i].clone())
            .collect();
        values.sort();
        values.dedup();
        Ok(values)
    }
    fn set_next_serial_value(
        &mut self,
        occurrence: u64,
        field: u64,
        value: &str,
    ) -> Result<(), DataError> {
        let mut state = self.0.lock().unwrap();
        state
            .events
            .push(format!("serial {occurrence} {field} {value}"));
        Ok(())
    }
    fn create_record(&mut self, _: u32, values: FieldChanges) -> Result<Record, DataError> {
        let mut state = self.0.lock().unwrap();
        let id = state.rows_mut().iter().map(|r| r.id).max().unwrap_or(0) + 1;
        let row = Record {
            id,
            values: FIELDS
                .iter()
                .map(|(name, _)| values.get(*name).cloned().flatten())
                .collect(),
        };
        state.rows_mut().push(row.clone());
        state.found.add(id);
        state.events.push("create".into());
        Ok(row)
    }
    fn delete_record(&mut self, _: u32, id: i64) -> Result<(), DataError> {
        let mut state = self.0.lock().unwrap();
        state.rows_mut().retain(|r| r.id != id);
        state.events.push(format!("delete {id}"));
        Ok(())
    }
    fn update_record(
        &mut self,
        _: u32,
        id: i64,
        values: FieldChanges,
    ) -> Result<Record, DataError> {
        // Taken before panicking, so the state's lock is not poisoned.
        let panics = std::mem::take(&mut self.0.lock().unwrap().panic_on_update);
        if panics {
            panic!("a provider defect (test)");
        }
        let mut state = self.0.lock().unwrap();
        let row = state
            .rows_mut()
            .iter_mut()
            .find(|r| r.id == id)
            .ok_or_else(|| DataError::new("Missing record"))?;
        for (name, value) in values {
            let i = FIELDS.iter().position(|(n, _)| *n == name).unwrap();
            row.values[i] = value;
        }
        let row = row.clone();
        state.events.push(format!("update {id}"));
        Ok(row)
    }
    fn unsettled_saves(&mut self) -> Vec<(u32, Vec<om_data::FileCommit>)> {
        // Counted down before panicking, so the state's lock is not
        // poisoned.
        let panics = {
            let mut state = self.0.lock().unwrap();
            let panics = state.panics_on_unsettled > 0;
            state.panics_on_unsettled = state.panics_on_unsettled.saturating_sub(1);
            panics
        };
        if panics {
            panic!("a provider defect (test)");
        }
        Vec::new()
    }
    fn calc_schema(&mut self) -> Result<Option<Arc<CalcSchema>>, DataError> {
        let file_options = {
            let state = self.0.lock().unwrap();
            if !state.calc_schema && state.file_options.is_none() {
                return Ok(None);
            }
            state.file_options.clone()
        };
        let mut schema = CalcSchema::new("Contacts", Locale::en_us());
        schema.add_table(OCCURRENCE, "Contacts");
        schema.add_occurrence(OCCURRENCE, "Contacts", OCCURRENCE);
        for (i, (name, kind)) in FIELDS.iter().enumerate() {
            schema.add_field(
                OCCURRENCE,
                FieldDef::data(i as u64 + 1, *name, value_type(*kind)),
            );
        }
        for (i, (name, kind, flags, field)) in SUMMARIES.iter().enumerate() {
            let value_type = if *kind == 9 {
                ValueType::Text
            } else {
                ValueType::Number
            };
            schema.add_field(
                OCCURRENCE,
                FieldDef {
                    class: FieldClass::Summary,
                    summary: Some(SummaryDef {
                        flags: *flags,
                        ..SummaryDef::new(SummaryKind::from_code(*kind), *field)
                    }),
                    ..FieldDef::data(summary_id(i), *name, value_type)
                },
            );
        }
        schema.file_options = file_options;
        // The layouts with their objects, for the Design and layout object
        // functions.
        for (id, name) in [(ENTRY, "Entry"), (OTHER, "Other")] {
            schema.add_layout_entry(LayoutEntry {
                layout: Some(Arc::new(render(id, Vec::new()).layout)),
                ..LayoutEntry::new(id, name, Some(OCCURRENCE))
            });
        }
        Ok(Some(Arc::new(schema)))
    }
    fn calc_data(&mut self, needs: &[CalcNeed]) -> Result<CalcData, DataError> {
        let schema = self.calc_schema()?.expect("a calculation schema");
        let locale = Locale::en_us();
        let cx = Ctx::new(&locale, Date::from_ymd(2026, 10, 29).unwrap());
        let mut source = MemorySource::new();
        let state = self.0.lock().unwrap();
        for row in &state.rows {
            let mut values = RecordValues::default();
            for (i, ((_, kind), value)) in FIELDS.iter().zip(&row.values).enumerate() {
                if let Some(text) = value {
                    values.set(i as u64 + 1, 1, stored_value(text, value_type(*kind), &cx));
                }
            }
            source.put_record(OCCURRENCE, row.id as u64, values);
        }
        // The window's found set, in its order.
        let found: Vec<u64> = state
            .found
            .page(fields(), &state.rows, 0, 1000)
            .records
            .iter()
            .map(|r| r.id as u64)
            .collect();
        source.set_found_set(OCCURRENCE, &found, None);
        drop(state);
        let recorder = Recorder::new(&source);
        for need in needs {
            recorder
                .fetch(&schema, need, &cx)
                .map_err(|e| DataError::new(format!("{need:?}: {e}")))?;
        }
        Ok(recorder.take())
    }
    fn render_layout(&mut self, id: u32) -> Result<Option<RenderLayout>, DataError> {
        let triggers = if id == ENTRY {
            self.0.lock().unwrap().triggers.clone()
        } else {
            Vec::new()
        };
        Ok(Some(render(id, triggers)))
    }
    fn close(&mut self) {}
}

fn value_type(kind: FieldKind) -> ValueType {
    match kind {
        FieldKind::Number => ValueType::Number,
        _ => ValueType::Text,
    }
}

fn render(id: u32, triggers: Vec<om_layout::ScriptTrigger>) -> RenderLayout {
    let title = LayoutObject {
        id: 1,
        object_type: 2,
        text: Some(
            if id == ENTRY {
                "ENTRY LAYOUT"
            } else {
                "OTHER LAYOUT"
            }
            .into(),
        ),
        bounds: Some(Rect {
            top: 5.0,
            left: 20.0,
            bottom: 35.0,
            right: 280.0,
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut objects = vec![title];
    let mut bindings = BTreeMap::new();
    for (i, (name, _)) in FIELDS.iter().enumerate() {
        let object = 10 + i as u64;
        let top = 60.0 + 40.0 * i as f64;
        objects.push(LayoutObject {
            id: object,
            object_type: 1,
            // Named for GetLayoutObjectAttribute ("Name field").
            name: (i == 0).then(|| "Name field".to_owned()),
            field_obj: Some(FieldObject {
                field_spec: Some(om_layout::FieldSpec {
                    table_key: OCCURRENCE,
                    field_key: i as u64 + 1,
                    repetition: 1,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            bounds: Some(Rect {
                top,
                left: 20.0,
                bottom: top + 30.0,
                right: 280.0,
                ..Default::default()
            }),
            ..Default::default()
        });
        bindings.insert(object, (*name).to_owned());
    }
    RenderLayout {
        layout: Layout {
            id: u64::from(id),
            name: if id == ENTRY { "Entry" } else { "Other" }.into(),
            def: Some(LayoutDef {
                // Fits a US Letter page between its default margins, so
                // Preview, Print and Save Records as PDF accept it.
                base_width: 540.0,
                table_alias_key: OCCURRENCE,
                ..Default::default()
            }),
            parts: vec![Part {
                id: 1,
                part_type: 4,
                top: 0.0,
                height: 300.0,
                ..Default::default()
            }],
            objects: ObjectStore {
                root: objects.iter().map(|o| o.id).collect(),
                objects,
                ..Default::default()
            },
            triggers,
            ..Default::default()
        },
        field_bindings: bindings,
    }
}

// ----- building scripts ---------------------------------------------------------

fn names() -> NameTable {
    let mut names = NameTable::default();
    names.add_occurrence(OCCURRENCE, "Contacts", OCCURRENCE);
    for (i, (name, _)) in FIELDS.iter().enumerate() {
        names.add_field(OCCURRENCE, i as u64 + 1, name);
    }
    for (i, (name, ..)) in SUMMARIES.iter().enumerate() {
        names.add_field(OCCURRENCE, summary_id(i), name);
        names.set_summary_field(OCCURRENCE, summary_id(i), true);
    }
    names
}

/// The field id of summary `i` of [`SUMMARIES`].
fn summary_id(i: usize) -> u64 {
    FIELDS.len() as u64 + 1 + i as u64
}

/// A step calculation compiled from source text.
fn calc(source: &str) -> Calculation {
    let names = names();
    let compiled = compile(source, &Context::new(&names)).expect("formula compiles");
    Calculation::new(
        Some(formatted::encode(&compiled.tokens)),
        polish::encode(&compiled.expr),
    )
}

fn field(name: &str) -> FieldSpec {
    let id = FIELDS.iter().position(|(n, _)| *n == name).unwrap() as u8 + 1;
    FieldSpec::new(
        StoredKey::new(&[1, OCCURRENCE as u8]).unwrap(),
        StoredKey::new(&[1, id]).unwrap(),
        1,
    )
}

fn spec_field(name: &str) -> SpecField {
    SpecField {
        occurrence: Some(OCCURRENCE),
        field: FIELDS.iter().position(|(n, _)| *n == name).unwrap() as u64 + 1,
        repetition: 1,
    }
}

fn step(id: u16) -> ScriptStep {
    ScriptStep::new(id)
}

fn set_field(name: &str, value: &str) -> ScriptStep {
    let mut s = step(76);
    s.set_field_spec(0, Some(field(name)));
    s.set_calculation(0, Some(&calc(value)));
    s
}

fn set_variable(name: &str, value: &str) -> ScriptStep {
    let mut s = step(141);
    s.set_text(0, Some(name));
    s.set_calculation(1, Some(&calc(value)));
    s
}

fn exit_script(result: &str) -> ScriptStep {
    let mut s = step(103);
    s.set_calculation(0, Some(&calc(result)));
    s
}

fn with_value(mut s: ScriptStep, value: u16) -> ScriptStep {
    s.value = value;
    s.flags |= flag::MENU_SELECTION;
    s
}

/// Export Records [No dialog; tab-separated UTF-8; no formatting] of Age
/// and `summaries` to `$p`.
fn export_ages(summaries: &[&str]) -> ScriptStep {
    use om_transfer::spec::{ExportField, ExportOptions, FieldSpec};
    let column = |field| ExportField {
        field: FieldSpec::new(OCCURRENCE, field),
        sum_by: FieldSpec::none(),
    };
    let mut fields = vec![column(spec_field("Age").field)];
    for name in summaries {
        let i = SUMMARIES.iter().position(|(n, ..)| n == name).unwrap();
        fields.push(column(summary_id(i)));
    }
    let options = ExportOptions {
        fields,
        group_by: Vec::new(),
        formatting: false,
        charset: om_transfer::ExportCharset::Utf8,
        use_name_alias: Some(1),
        field_names: None,
    };
    let profile = om_transfer::Profile::new(om_transfer::FileType::TabSeparated);
    let mut s = step(36);
    s.set_data_leaf(
        0x85,
        Some(om_data::runtime::OwnedNode::leaf([0x85], options.encode())),
    );
    s.set_data_leaf(
        0x86,
        Some(om_data::runtime::OwnedNode::leaf([0x86], profile.encode())),
    );
    s.flags |= flag::NO_INTERACT | flag::RESTORE | flag::EXTENDED_DATA | flag::FILE_PATH;
    s.set_data_leaf(0x82, Some(om_scripts::text_node(&[0x82], "$p")));
    s
}

fn script(id: u64, name: &str, steps: Vec<ScriptStep>) -> Script {
    Script {
        id,
        name: name.into(),
        steps,
        ..Default::default()
    }
}

/// Show Custom Dialog: OK (commits) and Cancel, input 1 into the Name
/// field and input 2 into `$answer` (script-steps.md 11.8).
fn custom_dialog() -> ScriptStep {
    let mut s = step(87);
    s.set_calculation(0, Some(&calc("\"Hello\"")));
    s.set_calculation(1, Some(&calc("\"Who are you?\"")));
    s.set_text(0, Some("OK"));
    s.set_text(1, Some("Cancel"));
    s.set_number(3, Some(1));
    s.set_number(4, Some(0));
    s.set_field_spec(0, Some(field("Name")));
    s.set_variable_target(1, "$answer");
    s.set_calculation(2, Some(&calc("\"Your name\"")));
    s.set_calculation(3, Some(&calc("\"Answer\"")));
    s
}

fn capture_errors() -> ScriptStep {
    let mut s = step(86);
    s.flags |= flag::TURN_ON;
    s
}

/// Open Transaction and Commit Transaction store the Commit's index.
fn transaction_step(id: u16, commit_at: u16) -> ScriptStep {
    let mut s = step(id);
    s.value = commit_at;
    s
}

/// Perform Script by name.
fn perform(name: &str) -> ScriptStep {
    let mut s = step(1);
    s.set_list_value(1, Some(1));
    s.set_calculation(2, Some(&calc(&format!("{name:?}"))));
    s
}

/// Go to Layout by name.
fn go_to_layout(name: &str) -> ScriptStep {
    let mut s = with_value(step(6), 3);
    s.set_calculation(5, Some(&calc(&format!("{name:?}"))));
    s
}

/// Revert Transaction with a custom error code and message.
fn revert_with(code: u32, message: &str) -> ScriptStep {
    let mut s = step(207);
    s.flags |= flag::OPTION2;
    s.set_calculation(1, Some(&calc(&code.to_string())));
    s.set_calculation(2, Some(&calc(&format!("{message:?}"))));
    s
}

/// A step with a path list (`82`).
fn path_step(id: u16, path: &str) -> ScriptStep {
    let mut s = step(id);
    s.flags |= flag::FILE_PATH;
    s.set_data_leaf(0x82, Some(om_data::text_node(&[0x82], path)));
    s
}

fn with_target_variable(mut s: ScriptStep, name: &str) -> ScriptStep {
    s.set_variable_target(0, name);
    s
}

/// The new scripts of this file's tests: transactions, data files, the
/// index, serials, a sort by value list and wildcard finds.
fn more_scripts() -> Vec<Script> {
    let file_id = || calc("$id");
    let mut write = with_target_variable(step(192), "$text");
    write.set_calculation(0, Some(&file_id()));
    write.value = 2;
    write.flags |= flag::OPTION2;
    let mut seek = step(195);
    seek.set_calculation(0, Some(&file_id()));
    seek.set_calculation(1, Some(&calc("6")));
    let mut read = step(193);
    read.set_calculation(0, Some(&file_id()));
    read.set_calculation(1, Some(&calc("5")));
    read.set_field_spec(0, Some(field("Note")));
    read.value = 2;
    let mut position = with_target_variable(step(194), "$pos");
    position.set_calculation(0, Some(&file_id()));
    let mut close = step(196);
    close.set_calculation(0, Some(&file_id()));
    let mut rename = path_step(199, "$path");
    rename.set_calculation(0, Some(&calc("\"om-gui-script-data2.txt\"")));
    let mut index = step(11);
    index.set_field_spec(0, Some(field("Name")));
    index.flags |= flag::SELECT_ALL;
    let mut serial = step(116);
    serial.set_field_spec(0, Some(field("Age")));
    serial.set_calculation(0, Some(&calc("\"100\"")));
    let by_list = SortSpec {
        keep_sorted: true,
        blanks_last: false,
        fields: vec![SortEntry {
            field: spec_field("Name"),
            summary: None,
            value_list: Some(7),
            descending: false,
            locale: 0,
        }],
    };
    let mut sort_step = step(39);
    sort_step.flags |= flag::RESTORE | flag::EXTENDED_DATA | flag::NO_INTERACT;
    sort_step.set_data_leaf(
        0x0a,
        Some(om_data::runtime::OwnedNode::leaf([0x0a], by_list.encode())),
    );
    vec![
        script(
            21,
            "Transaction",
            vec![
                capture_errors(),
                transaction_step(205, 6),
                set_field("Note", "\"first\""),
                with_value(step(16), 4),
                set_field("Note", "Get ( TransactionOpenState )"),
                step(62),
                transaction_step(206, 6),
            ],
        ),
        script(
            22,
            "Revert transaction",
            vec![
                capture_errors(),
                transaction_step(205, 4),
                set_field("Note", "\"x\""),
                step(207),
                transaction_step(206, 4),
                set_field("Note", "\"after \" & Get ( LastError )"),
                step(75),
            ],
        ),
        script(
            23,
            "Failing transaction",
            vec![
                capture_errors(),
                transaction_step(205, 3),
                set_field("Name", "\"invalid\""),
                transaction_step(206, 3),
                set_field("Note", "Get ( LastError )"),
                step(75),
            ],
        ),
        script(
            24,
            "Data files",
            vec![
                capture_errors(),
                set_variable(
                    "$path",
                    "\"file:\" & Get ( TemporaryPath ) & \"om-gui-script-data.txt\"",
                ),
                set_variable("$text", "\"Hello files\""),
                path_step(190, "$path"),
                with_target_variable(path_step(191, "$path"), "$id"),
                write,
                seek,
                read,
                position,
                set_variable("$info", "Get ( OpenDataFileInfo )"),
                close,
                with_target_variable(path_step(189, "$path"), "$size"),
                rename,
                set_variable(
                    "$path2",
                    "\"file:\" & Get ( TemporaryPath ) & \"om-gui-script-data2.txt\"",
                ),
                with_target_variable(path_step(188, "$path2"), "$exists"),
                path_step(197, "$path2"),
                with_target_variable(path_step(188, "$path2"), "$gone"),
                set_field(
                    "Note",
                    "Contacts::Note & \"|\" & $pos & \"|\" & PatternCount ( $info ; \"om-gui-script-data.txt\" ) & \"|\" & $size & \"|\" & $exists & \"|\" & $gone & \"|\" & Get ( LastError )",
                ),
                step(75),
            ],
        ),
        script(25, "Index and serial", vec![index, step(75), serial]),
        script(26, "Sort by list", vec![sort_step]),
        script(
            28,
            "Outer commits",
            vec![
                capture_errors(),
                transaction_step(205, 3),
                perform("Inner"),
                transaction_step(206, 3),
                set_field("Name", "Get ( LastError ) & \"/\" & $$inner"),
                step(75),
            ],
        ),
        script(
            29,
            "Inner",
            vec![
                transaction_step(205, 3),
                set_variable(
                    "$$inner",
                    "Get ( LastError ) & \",\" & Get ( TransactionOpenState )",
                ),
                set_field("Note", "\"inner\""),
                transaction_step(206, 3),
                set_variable("$$inner", "$$inner & \",\" & Get ( LastError )"),
            ],
        ),
        script(
            30,
            "Outer reverted",
            vec![
                capture_errors(),
                transaction_step(205, 4),
                set_field("Note", "\"outer\""),
                perform("Reverting inner"),
                transaction_step(206, 4),
                set_field(
                    "Name",
                    "Get ( LastError ) & \"/\" & IsEmpty ( $$skipped ) & \"/\" & \
                     Get ( TransactionOpenState )",
                ),
                step(75),
            ],
        ),
        script(
            31,
            "Reverting inner",
            vec![revert_with(5001, "custom message"), set_variable("$$skipped", "1")],
        ),
        script(
            32,
            "Commit inside",
            vec![
                capture_errors(),
                transaction_step(205, 5),
                set_field("Note", "\"x\""),
                step(75),
                set_field("Note", "\"y\""),
                transaction_step(206, 5),
                set_field("Name", "$$commits"),
            ],
        ),
        script(33, "Count commits", vec![set_variable("$$commits", "$$commits + 1")]),
        script(
            34,
            "Summaries",
            vec![
                set_field(
                    "Note",
                    "Contacts::Total Age & \"|\" & Contacts::Average Age & \"|\" & \
                     Contacts::Count Names & \"|\" & \
                     Substitute ( Contacts::Names ; \"¶\" ; \",\" ) & \"|\" & \
                     Contacts::Oldest & \"|\" & Contacts::Running Age & \"|\" & \
                     Round ( Contacts::Age Share ; 4 )",
                ),
                step(75),
            ],
        ),
        script(
            27,
            "Wildcard find",
            vec![
                step(22),
                set_field("Name", "\"Gr@ce\""),
                step(7),
                set_field("Name", "\"A*\""),
                step(28),
            ],
        ),
    ]
}

fn catalog() -> ScriptCatalog {
    let sort = SortSpec {
        keep_sorted: true,
        blanks_last: false,
        fields: vec![SortEntry {
            field: spec_field("Age"),
            summary: None,
            value_list: None,
            descending: true,
            locale: 0,
        }],
    };
    let mut sort_step = step(39);
    sort_step.flags |= flag::RESTORE | flag::EXTENDED_DATA | flag::NO_INTERACT;
    sort_step.set_data_leaf(
        0x0a,
        Some(om_data::runtime::OwnedNode::leaf([0x0a], sort.encode())),
    );
    let mut stored_find = step(28);
    stored_find.flags |= flag::RESTORE | flag::EXTENDED_DATA;
    stored_find.set_data_leaf(
        0x06,
        Some(encode_requests(&[StoredRequest {
            omit: false,
            criteria: vec![(spec_field("Name"), "==$x".into())],
        }])),
    );
    let mut missing_script = step(1);
    missing_script.set_list_value(1, Some(1));
    missing_script.set_calculation(2, Some(&calc("\"No such script\"")));
    let mut new_window = step(122);
    new_window.set_calculation(0, Some(&calc("\"Detail\"")));
    new_window.set_member_key(StoredKey::new(&[1, OTHER as u8]));
    let mut title = step(124);
    title.set_calculation(1, Some(&calc("\"Renamed\"")));
    let mut endless = vec![step(71), set_variable("$i", "$i + 1"), step(73)];
    endless[0].value = 2;
    endless[2].value = 0;
    ScriptCatalog {
        scripts: vec![
            script(
                1,
                "Ask",
                vec![
                    custom_dialog(),
                    set_field("Note", "Get ( LastMessageChoice ) & \"/\" & $answer"),
                    step(75),
                ],
            ),
            script(
                903,
                "Ask briefly",
                vec![
                    {
                        // "Close after" 1 second (script-steps.md 12.4).
                        let mut dialog = custom_dialog();
                        dialog.set_number_bool(6, Some(true));
                        dialog.set_calculation(13, Some(&calc("1")));
                        dialog
                    },
                    set_field("Note", "Get ( LastMessageChoice ) & \"/\" & $answer"),
                    step(75),
                ],
            ),
            script(
                2,
                "Missing layout",
                vec![
                    go_to_layout("Missing"),
                    set_field("Note", "\"after \" & Get ( LastError )"),
                    step(75),
                ],
            ),
            script(
                3,
                "Missing script",
                vec![
                    missing_script,
                    set_field("Note", "\"not reached\""),
                    step(75),
                ],
            ),
            script(4, "Refuse", vec![exit_script("False")]),
            script(
                5,
                "Commit by script",
                vec![set_field("Note", "\"scripted\""), step(75)],
            ),
            script(
                6,
                "Find and sort",
                vec![
                    step(22),
                    set_field("Name", "\"a\""),
                    step(7),
                    set_field("Age", "\">70\""),
                    step(28),
                    sort_step,
                    with_value(step(16), 1),
                ],
            ),
            script(
                7,
                "Find by variable",
                vec![set_variable("$x", "\"Barbara\""), stored_find],
            ),
            script(
                8,
                "Windows",
                vec![
                    new_window,
                    title,
                    with_value(step(30), 2),
                    with_value(step(29), 2),
                    step(62),
                    step(121),
                ],
            ),
            script(9, "Endless", endless),
            script(10, "Go to Other", vec![go_to_layout("Other")]),
            script(
                90,
                "Layout objects",
                vec![
                    set_field(
                        "Note",
                        "LayoutObjectNames ( \"\" ; \"Entry\" ) & \"|\" & \
                         GetLayoutObjectAttribute ( \"name field\" ; \"width\" ) & \" \" & \
                         GetLayoutObjectAttribute ( \"name field\" ; \"height\" ) & \"|\" & \
                         GetLayoutObjectAttribute ( \"Name field\" ; \"content\" ) & \"|\" & \
                         GetLayoutObjectAttribute ( \"Name field\" ; \"source\" ) & \"|\" & \
                         GetLayoutObjectAttribute ( \"Name field\" ; \"objectType\" ) & \"|\" & \
                         LayoutNames ( \"\" ) & \"|\" & Get ( LayoutNumber )",
                    ),
                    step(75),
                ],
            ),
            script(
                11,
                "Query",
                vec![
                    set_field(
                        "Note",
                        "ExecuteSQL ( \"SELECT COUNT(*) FROM Contacts WHERE Age > ?\" ; \"\" ; \"\" ;                          40 ) & \"/\" & ExecuteSQL ( \"SELECT Name FROM Contacts WHERE Name LIKE                          'B%'\" ; \"\" ; \"\" )",
                    ),
                    step(75),
                ],
            ),
        ]
        .into_iter()
        .chain(more_scripts())
        .collect(),
        writable: false,
        unavailable_reason: None,
    }
}

fn rows() -> Vec<Record> {
    rows_of(&[
        ("Ada", "36"),
        ("Grace", "85"),
        ("Alan", "41"),
        ("Barbara", "70"),
        ("Edsger", "72"),
    ])
}

/// Records of names and ages, in creation order.
fn rows_of(people: &[(&str, &str)]) -> Vec<Record> {
    people
        .iter()
        .enumerate()
        .map(|(i, (name, age))| Record {
            id: i as i64 + 1,
            values: vec![Some((*name).into()), Some((*age).into()), None],
        })
        .collect()
}

/// A trigger record running `script` in Browse mode.
fn trigger(event: TriggerEvent, script: u64) -> om_layout::ScriptTrigger {
    om_layout::ScriptTrigger {
        event: event.id(),
        script: Some(script),
        file_reference: None,
        external: false,
        modes: 1,
        option: 0,
        extra: Vec::new(),
        parameter: None,
    }
}

fn open(triggers: Vec<om_layout::ScriptTrigger>) -> (common::App, Arc<Mutex<State>>) {
    open_with(State {
        triggers,
        ..Default::default()
    })
}

/// Open the file as `state` describes it, with [`rows`] unless it has rows
/// of its own (the first named Ada); its scripts are added to the
/// catalog's.
fn open_with(mut state: State) -> (common::App, Arc<Mutex<State>>) {
    let mut scripts = catalog();
    scripts
        .scripts
        .extend(state.scripts.scripts.iter().cloned());
    let rows = match std::mem::take(&mut state.rows) {
        own if own.is_empty() => rows(),
        own => own,
    };
    let state = Arc::new(Mutex::new(State {
        rows,
        scripts,
        ..state
    }));
    let mut app = common::harness(Box::new(Provider(state.clone())));
    common::fill(&mut app, "Host", "scripts.example.invalid");
    common::fill(&mut app, "File", "Contacts.fmp12");
    common::click(&mut app, "Open");
    common::wait_for_label(&mut app, "ENTRY LAYOUT");
    common::wait_until(&mut app, "first record", |app| {
        common::find_text_field(app, "Name")
            .and_then(|f| f.value())
            .as_deref()
            == Some("Ada")
    });
    common::open_menu(&mut app, "Scripts");
    common::wait_for_label(&mut app, "Ask");
    // Escape closes the menu, but also stops a running script. Let the
    // file's opening triggers finish before sending it.
    common::wait_until(&mut app, "the opening triggers to finish", |app| {
        app.state().is_settled()
    });
    app.key_press(egui::Key::Escape);
    app.run_steps(2);
    (app, state)
}

/// Replace the text of the field labelled `label`.
fn replace(app: &mut common::App, label: &str, text: &str) {
    common::text_field(app, label).focus();
    app.step();
    app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    common::text_field(app, label).type_text(text);
    app.run_steps(2);
}

fn run(app: &mut common::App, name: &str) {
    common::open_menu(app, "Scripts");
    common::click(app, name);
}

/// With `OM_SCREENSHOTS` set to a folder, render the window there (the
/// dialogs, for review).
fn shot(app: &mut common::App, name: &str) {
    let Some(dir) = std::env::var_os("OM_SCREENSHOTS") else {
        return;
    };
    app.run_steps(2);
    app.set_render_cursor(false);
    let image = app.render().expect("offscreen render");
    image
        .save(std::path::Path::new(&dir).join(format!("{name}.png")))
        .expect("write png");
}

fn wait_idle(app: &mut common::App) {
    common::wait_until(app, "the script to end", |app| {
        !app.state().script_running()
    });
}

fn note(state: &Arc<Mutex<State>>) -> Option<String> {
    state.lock().unwrap().rows[0].values[2].clone()
}

#[test]
fn custom_dialogs_store_inputs_and_the_chosen_button() {
    let (mut app, state) = open(Vec::new());
    run(&mut app, "Ask");
    common::wait_for_label(&mut app, "Who are you?");
    // Inputs start from the field's value.
    assert_eq!(
        common::text_field(&app, "Your name").value().as_deref(),
        Some("Ada")
    );
    replace(&mut app, "Your name", "Ada L.");
    common::fill(&mut app, "Answer", "yes");
    common::click(&mut app, "OK");
    wait_idle(&mut app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    let row = state.lock().unwrap().rows[0].clone();
    assert_eq!(row.values[0].as_deref(), Some("Ada L."));
    assert_eq!(row.values[2].as_deref(), Some("1/yes"));

    // Cancel stores no input; Get ( LastMessageChoice ) is 2, and the
    // last run's `$answer` was local to it.
    run(&mut app, "Ask");
    common::wait_for_label(&mut app, "Who are you?");
    common::fill(&mut app, "Your name", " ignored");
    common::click(&mut app, "Cancel");
    wait_idle(&mut app);
    let row = state.lock().unwrap().rows[0].clone();
    assert_eq!(row.values[0].as_deref(), Some("Ada L."));
    assert_eq!(row.values[2].as_deref(), Some("2/"));

    // "Close after": the dialog closes by itself as no button, 0, with no
    // click. Its inputs are left to the runtime's test: under load the
    // dialog may close before this one could type.
    run(&mut app, "Ask briefly");
    common::wait_until(&mut app, "the dialog's timeout", |_| {
        state.lock().unwrap().rows[0].values[2].as_deref() == Some("0/")
    });
    wait_idle(&mut app);
    assert!(app.query_by_label("Who are you?").is_none());
    let row = state.lock().unwrap().rows[0].clone();
    assert_eq!(row.values[0].as_deref(), Some("Ada L."));
    assert_eq!(row.values[2].as_deref(), Some("0/"));
}

#[test]
fn scripts_query_the_file_with_execute_sql() {
    let (mut app, state) = open_with(State {
        calc_schema: true,
        ..Default::default()
    });
    run(&mut app, "Query");
    wait_idle(&mut app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    // Ages over 40: Grace, Alan, Barbara, Edsger.
    assert_eq!(note(&state).as_deref(), Some("4/Barbara"));
}

#[test]
fn scripts_read_layout_objects_and_the_design_functions() {
    let (mut app, state) = open_with(State {
        calc_schema: true,
        ..Default::default()
    });
    run(&mut app, "Layout objects");
    wait_idle(&mut app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    // The named field object of the window's layout on the first record
    // (its screen position: tests/get_selectors.rs).
    assert_eq!(
        note(&state).as_deref(),
        Some("Name field|260 30|Ada|Contacts::Name|field|Entry\rOther|1")
    );
}

#[test]
fn error_alerts_continue_or_cancel_the_script() {
    let (mut app, state) = open(Vec::new());
    // 105 is a plain alert: OK continues, and the script sees the error.
    run(&mut app, "Missing layout");
    common::wait_until(&mut app, "layout alert", |app| {
        app.state().error().is_some_and(|e| e.contains("105"))
    });
    common::click(&mut app, "OK");
    wait_idle(&mut app);
    assert_eq!(note(&state).as_deref(), Some("after 105"));

    // 104 asks Continue or Cancel; Cancel halts before the next step.
    run(&mut app, "Missing script");
    common::wait_until(&mut app, "script alert", |app| {
        app.state().error().is_some_and(|e| e.contains("104"))
    });
    common::click(&mut app, "Cancel");
    wait_idle(&mut app);
    assert_eq!(note(&state).as_deref(), Some("after 105"));
    assert!(app.state().error().is_none());
}

#[test]
fn an_on_record_commit_trigger_cancels_the_commit() {
    let refuse = trigger(TriggerEvent::RecordCommit, 4);
    let (mut app, state) = open(vec![refuse]);
    // The user's own commit.
    common::fill(&mut app, "Name", " changed");
    common::wait_until(&mut app, "the record\'s edit", common::editing);
    common::save_record(&mut app);
    wait_idle(&mut app);
    app.run_steps(4);
    assert!(
        state.lock().unwrap().events.is_empty(),
        "the commit was cancelled"
    );
    assert_eq!(
        common::text_field(&app, "Name").value().as_deref(),
        Some("Ada changed"),
        "the edit stays unsaved"
    );
    // A script's Commit Records fires the same trigger.
    common::revert_record(&mut app);
    app.run_steps(4);
    run(&mut app, "Commit by script");
    wait_idle(&mut app);
    assert!(state.lock().unwrap().events.is_empty());
    assert_eq!(note(&state), None);
}

/// File > Import Records > File, Export Records and Save/Send Records As >
/// Excel and PDF: the record's commit and OnRecordCommit go first, then
/// the step's own dialog.
#[test]
fn menu_transfers_obey_commit_veto_and_provider_refusal() {
    let commands = [
        ("Export Records", &["Export Records..."][..]),
        (
            "Import Records from File",
            &["Import Records", "File..."][..],
        ),
        (
            "Save Records as Excel",
            &["Save/Send Records As", "Excel..."][..],
        ),
        (
            "Save Records as PDF",
            &["Save/Send Records As", "PDF..."][..],
        ),
    ];
    for (dialog, path) in commands {
        let import = dialog.starts_with("Import");
        let invoke = |app: &mut common::App| {
            common::open_menu(app, "File");
            for item in path {
                common::click(app, item);
            }
        };
        let gate = script(
            1001,
            "Transfer commit gate",
            vec![exit_script("Contacts::Name = \"allowed\"")],
        );
        let (mut app, state) = open_with(State {
            calc_schema: true,
            import_supported: true,
            triggers: vec![trigger(TriggerEvent::RecordCommit, gate.id)],
            scripts: ScriptCatalog {
                scripts: vec![gate],
                ..Default::default()
            },
            ..Default::default()
        });
        replace(&mut app, "Name", "blocked");
        invoke(&mut app);
        wait_idle(&mut app);
        assert!(app.query_by_label(dialog).is_none());
        assert!(state.lock().unwrap().events.is_empty());
        assert_eq!(
            common::text_field(&app, "Name").value().as_deref(),
            Some("blocked")
        );

        replace(&mut app, "Name", "allowed");
        invoke(&mut app);
        common::wait_for_label(&mut app, dialog);
        assert_eq!(state.lock().unwrap().events, ["update 1"]);
        assert_eq!(
            state.lock().unwrap().rows[0].values[0].as_deref(),
            Some("allowed")
        );
        common::click(&mut app, "Cancel");
        wait_idle(&mut app);
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        if dialog.starts_with("Save") {
            // Their refusals are the steps' own (portable-pdf.md,
            // script_transfer.rs).
            continue;
        }

        // Export requires calculation access. Import also requires an
        // actual import capability, even when calculation access exists.
        let (mut app, _state) = open_with(State {
            calc_schema: import,
            ..Default::default()
        });
        invoke(&mut app);
        common::wait_until(&mut app, "unsupported export", |app| {
            app.state().error().is_some_and(|e| {
                e.contains(if import {
                    "Import Records"
                } else {
                    "Export Records"
                }) && e.contains("not supported")
            })
        });
        assert!(app.query_by_label(dialog).is_none());
        common::click(&mut app, "OK");
        wait_idle(&mut app);
    }
}

/// The Preview toolbar's Save as PDF and the Browse toolbar's Share > Save
/// as Excel write the found set through the steps' dialogs, and Save as
/// Excel of the current record writes that record alone. Each row of the
/// found set's workbook, and of a script's Export Records, holds its own
/// record's summaries, as FileMaker Pro 26.0.3 wrote these three ages'
/// total, running total and fraction of total (#360). Set
/// `OM_SAVE_RECORDS_ARTIFACT_DIR` to keep the files.
#[test]
fn the_toolbars_save_the_found_set_as_pdf_and_excel() {
    let tab = format!("om-gui-summaries-{}.tab", std::process::id());
    let export = script(
        151,
        "Export ages",
        vec![
            set_variable(
                "$p",
                &format!("\"file:\" & Get ( TemporaryPath ) & \"{tab}\""),
            ),
            export_ages(&["Total Age", "Running Age", "Age Share"]),
        ],
    );
    let (mut app, _state) = open_with(State {
        calc_schema: true,
        rows: rows_of(&[("Ada", "10"), ("Grace", "20"), ("Alan", "30")]),
        scripts: ScriptCatalog {
            scripts: vec![export],
            ..Default::default()
        },
        ..Default::default()
    });
    let kept = std::env::var_os("OM_SAVE_RECORDS_ARTIFACT_DIR").map(std::path::PathBuf::from);
    let temporary = tempfile::tempdir().unwrap();
    let dir = kept.unwrap_or_else(|| temporary.path().to_owned());
    std::fs::create_dir_all(&dir).unwrap();
    // The dialog, its path field, buttons to click first, and the file.
    // Screenshots: `save-<file>-opened` as the dialog opens, and
    // `save-<file>-chosen` after those clicks.
    let save = |app: &mut common::App, dialog: &str, field: &str, first: &[&str], name: &str| {
        common::wait_for_label(app, dialog);
        let stem = format!("save-{}", name.replace('.', "-"));
        shot(app, &format!("{stem}-opened"));
        for button in first {
            common::click(app, button);
        }
        if !first.is_empty() {
            shot(app, &format!("{stem}-chosen"));
        }
        let path = dir.join(name);
        common::text_field(app, field).focus();
        app.step();
        app.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        common::text_field(app, field).type_text(path.to_str().unwrap());
        app.run_steps(2);
        common::click(app, "Save");
        // A refusal waits on its alert: report it rather than time out.
        common::wait_until(app, "the save", |app| {
            !app.state().script_running() || app.state().error().is_some()
        });
        assert!(app.state().error().is_none(), "{:?}", app.state().error());
        std::fs::read(path).unwrap()
    };

    common::click(&mut app, "Preview");
    common::click(&mut app, "Save as PDF");
    let pdf = save(
        &mut app,
        "Save Records as PDF",
        "Save As:",
        &[],
        "found-set.pdf",
    );
    assert!(pdf.starts_with(b"%PDF"));
    common::click(&mut app, "Exit Preview");

    common::click(&mut app, "Share");
    common::click(&mut app, "Save as Excel...");
    let xlsx = save(
        &mut app,
        "Save Records as Excel",
        "Save as:",
        // This schema lists no layouts, so no fields start chosen.
        &["Move All"],
        "found-set.xlsx",
    );
    let rows = |bytes: &[u8]| om_transfer::import::read::workbook(bytes, 0, "").unwrap();
    // A header row of field names, then the three records.
    let found = rows(&xlsx);
    assert_eq!(found.len(), 4, "{found:?}");
    let column = |name: &str| -> Vec<String> {
        let i = found[0].iter().position(|c| *c == [name]).unwrap();
        found[1..].iter().map(|row| row[i].concat()).collect()
    };
    assert_eq!(column("Age"), ["10", "20", "30"]);
    assert_eq!(column("Total Age"), ["60", "60", "60"]);
    assert_eq!(column("Running Age"), ["10", "30", "60"]);
    assert_eq!(
        column("Age Share"),
        [".1666666666666667", ".3333333333333333", ".5"]
    );

    // On the second record, "Current record" writes that record alone.
    common::click(&mut app, "Next");
    common::wait_until(&mut app, "the second record", |app| {
        common::text_field(app, "Name").value().as_deref() == Some("Grace")
    });
    common::click(&mut app, "Share");
    common::click(&mut app, "Save as Excel...");
    let xlsx = save(
        &mut app,
        "Save Records as Excel",
        "Save as:",
        &["Move All", "Current record"],
        "current-record.xlsx",
    );
    let rows = rows(&xlsx);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(rows[1][0], ["Grace"]);

    // Export Records of the found set, on the second record, writes the
    // same values as FileMaker Pro's tab-separated export.
    run(&mut app, "Export ages");
    wait_idle(&mut app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    let exported = std::env::temp_dir()
        .join(format!("OpenMaker-S{}", std::process::id()))
        .join(&tab);
    let written = std::fs::read_to_string(&exported).unwrap();
    let _ = std::fs::remove_file(&exported);
    std::fs::write(dir.join("found-set.tab"), &written).unwrap();
    assert_eq!(
        written,
        "10\t60\t10\t.1666666666666667\r20\t60\t30\t.3333333333333333\r30\t60\t60\t.5\r"
    );
}

#[test]
fn a_find_and_sort_script_shows_the_sorted_found_set() {
    let (mut app, _state) = open(Vec::new());
    run(&mut app, "Find and sort");
    wait_idle(&mut app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    // Names starting with "a" or ages over 70, oldest first.
    common::wait_until(&mut app, "sorted found set", |app| {
        app.query_by_label("Found (Sorted)").is_some()
            && common::find_text_field(app, "Name")
                .and_then(|f| f.value())
                .as_deref()
                == Some("Grace")
    });
    app.get_by_label("4");
    assert_eq!(common::record_number(&app), "1");
    common::click(&mut app, "Next");
    common::wait_until(&mut app, "second record", |app| {
        common::find_text_field(app, "Name")
            .and_then(|f| f.value())
            .as_deref()
            == Some("Edsger")
    });

    // A stored request with a variable: Name "==$x", $x = "Barbara".
    run(&mut app, "Find by variable");
    wait_idle(&mut app);
    common::wait_until(&mut app, "one record", |app| {
        app.query_by_label("Found (Unsorted)").is_some()
            && common::find_text_field(app, "Name")
                .and_then(|f| f.value())
                .as_deref()
                == Some("Barbara")
    });
    app.get_by_label("1");
}

#[test]
fn window_steps_open_retitle_and_close_a_window_around_a_pause() {
    let (mut app, _state) = open(Vec::new());
    run(&mut app, "Windows");
    common::wait_until(&mut app, "paused", |app| app.state().script_paused());
    // List view: the layout's title on every row.
    common::wait_until(&mut app, "the other layout", |app| {
        app.query_all_by_label("OTHER LAYOUT").next().is_some()
    });
    // The new window is in front, its toolbar hidden; the file's first
    // window stays open on its layout, with its toolbar.
    assert_eq!(app.state().window_name(), "Renamed");
    assert_eq!(app.state().view(), om_gui::ViewMode::List);
    assert_eq!(app.query_all_by_label("Edit Layout").count(), 1);
    app.get_by_label("ENTRY LAYOUT");
    common::click(&mut app, "Continue");
    wait_idle(&mut app);
    // Close Window closes it: the first window is in front again.
    common::wait_until(&mut app, "one window", |app| {
        app.query_all_by_label("OTHER LAYOUT").next().is_none()
    });
    app.get_by_label("ENTRY LAYOUT");
    assert_eq!(
        app.state().window_name(),
        "Contacts (scripts.example.invalid)"
    );
    app.get_by_label("Edit Layout");
}

#[test]
fn script_zoom_in_and_out_follow_the_document_zoom_levels() {
    let mut steps = Vec::new();
    let choices = [3, 9, 10, 10, 10, 10, 9, 9, 9, 9, 9, 9, 9, 9, 4];
    let expected = [
        75, 100, 75, 50, 25, 25, 50, 75, 100, 150, 200, 300, 400, 400, 100,
    ];
    for (index, choice) in choices.into_iter().enumerate() {
        steps.push(with_value(step(97), choice));
        let mut title = step(124);
        title.set_calculation(1, Some(&calc(&format!("\"Zoom {index}\""))));
        steps.extend([title, step(62)]);
    }
    let (mut app, _state) = open_with(State {
        scripts: ScriptCatalog {
            scripts: vec![script(150, "Zoom levels", steps)],
            ..Default::default()
        },
        ..Default::default()
    });
    let width = common::text_field(&app, "Name").rect().width();
    run(&mut app, "Zoom levels");
    for (index, percent) in expected.into_iter().enumerate() {
        common::wait_until(&mut app, "the zoom step to pause", |app| {
            app.state().script_paused() && app.state().window_name() == format!("Zoom {index}")
        });
        let actual = common::text_field(&app, "Name").rect().width();
        assert!(
            (actual - width * percent as f32 / 100.0).abs() < 0.1,
            "step {index}: expected {percent}% width, got {actual} (100% width {width})"
        );
        common::click(&mut app, "Continue");
    }
    wait_idle(&mut app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
}

#[test]
fn escape_stops_a_running_script() {
    let (mut app, _state) = open(Vec::new());
    run(&mut app, "Endless");
    common::wait_until(&mut app, "running", |app| app.state().script_running());
    app.run_steps(3);
    assert!(app.state().script_running());
    app.key_press(egui::Key::Escape);
    wait_idle(&mut app);
}

#[test]
fn an_on_layout_exit_trigger_keeps_the_user_on_the_layout() {
    let refuse = trigger(TriggerEvent::LayoutExit, 4);
    let (mut app, _state) = open(vec![refuse]);
    common::select_layout(&mut app, "Other");
    wait_idle(&mut app);
    app.run_steps(4);
    app.get_by_label("ENTRY LAYOUT");
    assert!(app.query_by_label("OTHER LAYOUT").is_none());
    // A script's Go to Layout passes the same trigger.
    run(&mut app, "Go to Other");
    wait_idle(&mut app);
    app.run_steps(4);
    app.get_by_label("ENTRY LAYOUT");
    assert!(app.query_by_label("OTHER LAYOUT").is_none());
}

fn notes(state: &Arc<Mutex<State>>) -> Vec<Option<String>> {
    state
        .lock()
        .unwrap()
        .rows
        .iter()
        .map(|r| r.values[2].clone())
        .collect()
}

#[test]
fn transactions_hold_changes_until_commit_transaction() {
    let (mut app, state) = open(Vec::new());
    run(&mut app, "Transaction");
    common::wait_until(&mut app, "paused", |app| app.state().script_paused());
    // Both records were left (committed into the transaction), but nothing
    // is written before Commit Transaction.
    assert_eq!(notes(&state)[..2], [None, None]);
    assert_eq!(
        state.lock().unwrap().events,
        ["begin", "update 1"],
        "the second record's change is still the window's"
    );
    common::click(&mut app, "Continue");
    wait_idle(&mut app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(notes(&state)[..2], [Some("first".into()), Some("1".into())]);
    assert_eq!(
        state.lock().unwrap().events,
        ["begin", "update 1", "update 2", "commit"]
    );
}

#[test]
fn reverted_and_failed_transactions_leave_the_records() {
    let (mut app, state) = open(Vec::new());
    run(&mut app, "Revert transaction");
    wait_idle(&mut app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(notes(&state)[0].as_deref(), Some("after 0"));
    assert!(state.lock().unwrap().events.contains(&"revert".to_owned()));

    run(&mut app, "Failing transaction");
    wait_idle(&mut app);
    let row = state.lock().unwrap().rows[0].clone();
    assert_eq!(row.values[0].as_deref(), Some("Ada"), "nothing was written");
    assert_eq!(row.values[2].as_deref(), Some("507"));
    assert!(state.lock().unwrap().events.contains(&"refused".to_owned()));
}

#[test]
fn a_sub_scripts_transaction_steps_return_3_and_its_changes_join_the_callers() {
    let (mut app, state) = open(Vec::new());
    run(&mut app, "Outer commits");
    wait_idle(&mut app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    // Inside, Open and Commit Transaction are 3 and the transaction stays
    // open; the caller's Commit Transaction writes the sub-script's change.
    let row = state.lock().unwrap().rows[0].clone();
    assert_eq!(row.values[0].as_deref(), Some("0/3,1,3"));
    assert_eq!(row.values[2].as_deref(), Some("inner"));
    assert!(state.lock().unwrap().events.contains(&"commit".to_owned()));
}

#[test]
fn a_sub_scripts_revert_reverts_the_callers_transaction_with_its_code() {
    let (mut app, state) = open(Vec::new());
    run(&mut app, "Outer reverted");
    wait_idle(&mut app);
    // The rest of the sub-script is skipped; Commit Transaction reverts and
    // returns the custom code (script-runtime.md 13.10).
    let row = state.lock().unwrap().rows[0].clone();
    assert_eq!(row.values[0].as_deref(), Some("5001/1/0"));
    assert_eq!(row.values[2], None, "the caller's change was reverted");
    let events = state.lock().unwrap().events.clone();
    assert!(events.contains(&"revert".to_owned()), "{events:?}");
    assert!(!events.contains(&"commit".to_owned()), "{events:?}");
}

#[test]
fn on_record_commit_fires_when_a_record_is_left_not_at_commit_transaction() {
    let count = om_layout::ScriptTrigger {
        event: TriggerEvent::RecordCommit.id(),
        script: Some(33),
        file_reference: None,
        external: false,
        modes: 1,
        option: 0,
        extra: Vec::new(),
        parameter: None,
    };
    let (mut app, state) = open(vec![count]);
    run(&mut app, "Commit inside");
    wait_idle(&mut app);
    // Commit Records inside the transaction fired it once; Commit
    // Transaction committed the second change without it (8.2).
    let row = state.lock().unwrap().rows[0].clone();
    assert_eq!(row.values[2].as_deref(), Some("y"));
    assert_eq!(
        common::text_field(&app, "Name").value().as_deref(),
        Some("1")
    );
}

#[test]
fn data_file_steps_write_read_rename_and_delete() {
    let (mut app, state) = open(Vec::new());
    run(&mut app, "Data files");
    wait_idle(&mut app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    // "Hello files" and a line feed is 12 bytes; 5 bytes from position 6
    // are "files", which leaves the position at 11.
    assert_eq!(notes(&state)[0].as_deref(), Some("files|11|1|12|1|0|0"));
}

#[test]
fn insert_from_index_pastes_the_chosen_value_and_serials_reach_the_host() {
    let (mut app, state) = open(Vec::new());
    run(&mut app, "Index and serial");
    common::wait_for_label(&mut app, "Grace");
    common::click(&mut app, "Grace");
    common::click(&mut app, "Paste");
    wait_idle(&mut app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    let state = state.lock().unwrap();
    assert_eq!(state.rows[0].values[0].as_deref(), Some("Grace"));
    assert_eq!(
        state.events.last().map(String::as_str),
        Some("serial 5 2 100")
    );
}

#[test]
fn a_value_list_sort_and_a_wildcard_find() {
    let (mut app, _state) = open(Vec::new());
    run(&mut app, "Sort by list");
    wait_idle(&mut app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    // The list's values first (Edsger, Ada), then the others by name.
    common::wait_until(&mut app, "sorted by the value list", |app| {
        common::find_text_field(app, "Name")
            .and_then(|f| f.value())
            .as_deref()
            == Some("Edsger")
    });
    common::click(&mut app, "Next");
    common::wait_until(&mut app, "the list's second value", |app| {
        common::find_text_field(app, "Name")
            .and_then(|f| f.value())
            .as_deref()
            == Some("Ada")
    });
    common::click(&mut app, "Next");
    common::wait_until(&mut app, "the rest by name", |app| {
        common::find_text_field(app, "Name")
            .and_then(|f| f.value())
            .as_deref()
            == Some("Alan")
    });

    run(&mut app, "Wildcard find");
    wait_idle(&mut app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    common::wait_until(&mut app, "three records", |app| {
        app.query_by_label("Found (Unsorted)").is_some()
            && common::find_text_field(app, "Name")
                .and_then(|f| f.value())
                .as_deref()
                == Some("Ada")
    });
    app.get_by_label("3");
}

/// A Perform Script step's script id from 128 on is a two-byte serial key;
/// these scripts use 141 and up to check the whole path.
#[test]
fn a_layout_trigger_record_runs_its_script_with_its_parameter() {
    // OnLayoutEnter, script 141 (`80 0d`), Browse: the 17-byte record as
    // FileMaker stores it, and its parameter at `31`.
    let record = [
        0x00, 0x65, 0x03, 0x02, 0x80, 0x0d, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00,
    ];
    let mut on_enter = om_layout::triggers::decode_record(&record).unwrap();
    assert_eq!(on_enter.script, Some(141));
    on_enter.parameter = Some(om_layout::triggers::StoredFormula {
        context: None,
        polish: calc("\"entered \" & 2 * 3").polish.unwrap(),
    });
    let scripts = vec![script(
        141,
        "Note the parameter",
        vec![set_field("Note", "Get ( ScriptParameter )"), step(75)],
    )];
    let (mut app, state) = open_with(State {
        triggers: vec![on_enter],
        scripts: extra(scripts),
        ..Default::default()
    });
    wait_idle(&mut app);
    common::wait_until(&mut app, "the trigger's commit", |_| {
        note(&state).as_deref() == Some("entered 6")
    });
}

/// A script catalog holding `scripts`, for [`open_with`].
fn extra(scripts: Vec<Script>) -> ScriptCatalog {
    ScriptCatalog {
        scripts,
        ..ScriptCatalog::default()
    }
}

#[test]
fn the_file_triggers_run_when_the_window_opens() {
    // The file's document preferences name OnFirstWindowOpen 142 and
    // OnWindowOpen 143; they run before the first layout's OnLayoutEnter.
    let options = FileOptions {
        first_window_open: Some(142),
        window_open: Some(143),
        ..FileOptions::default()
    };
    let scripts = vec![
        script(142, "First", vec![set_variable("$$order", "\"first\"")]),
        script(
            143,
            "Window",
            vec![set_variable("$$order", "$$order & \",window\"")],
        ),
        script(
            144,
            "Layout",
            vec![set_field("Note", "$$order & \",layout\""), step(75)],
        ),
    ];
    let (mut app, state) = open_with(State {
        triggers: vec![trigger(TriggerEvent::LayoutEnter, 144)],
        scripts: extra(scripts),
        file_options: Some(options),
        ..Default::default()
    });
    wait_idle(&mut app);
    common::wait_until(&mut app, "the triggers in order", |_| {
        note(&state).as_deref() == Some("first,window,layout")
    });
}

/// Summary fields in a script's calculations are computed over the
/// window's found set in its order (`om_data::summary`): a total, an
/// average, a count, a list, a maximum, a running total and a fraction on
/// the current record, before and after a find and a sort.
#[test]
fn summaries_follow_the_windows_found_set() {
    let (mut app, state) = open_with(State {
        calc_schema: true,
        ..Default::default()
    });
    // Ada 36, Grace 85, Alan 41, Barbara 70, Edsger 72; on Ada.
    run(&mut app, "Summaries");
    wait_idle(&mut app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(
        note(&state).as_deref(),
        Some("304|60.8|5|Ada,Grace,Alan,Barbara,Edsger|85|36|.1184")
    );
    // Names with "a" or over 70, oldest first: Grace, Edsger, Alan, Ada.
    run(&mut app, "Find and sort");
    wait_idle(&mut app);
    common::wait_until(&mut app, "sorted found set", |app| {
        common::find_text_field(app, "Name")
            .and_then(|f| f.value())
            .as_deref()
            == Some("Grace")
    });
    run(&mut app, "Summaries");
    wait_idle(&mut app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    let grace = state.lock().unwrap().rows[1].values[2].clone();
    assert_eq!(
        grace.as_deref(),
        Some("234|58.5|4|Grace,Edsger,Alan,Ada|85|85|.3632")
    );
}

/// Go to Layout calls OnRecordLoad before OnLayoutEnter and each trigger's
/// frame goes on top, so OnLayoutEnter's script runs first
/// (script-runtime.md 13.17); the window's own layout change runs them in
/// the same order.
#[test]
fn on_layout_enter_runs_before_on_record_load() {
    let scripts = vec![
        script(
            147,
            "Entered",
            vec![set_variable("$$order", "$$order & \",enter\"")],
        ),
        script(
            148,
            "Loaded",
            vec![set_variable("$$order", "$$order & \",load\"")],
        ),
        script(
            149,
            "Round trip",
            vec![
                set_field("Note", "$$order & \"|\""),
                set_variable("$$order", "\"\""),
                go_to_layout("Other"),
                go_to_layout("Entry"),
                set_field("Note", "Contacts::Note & $$order"),
                step(75),
            ],
        ),
    ];
    let (mut app, state) = open_with(State {
        triggers: vec![
            trigger(TriggerEvent::LayoutEnter, 147),
            trigger(TriggerEvent::RecordLoad, 148),
        ],
        scripts: extra(scripts),
        ..Default::default()
    });
    wait_idle(&mut app);
    run(&mut app, "Round trip");
    wait_idle(&mut app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    common::wait_until(&mut app, "both layout changes' triggers", |_| {
        note(&state).is_some()
    });
    assert_eq!(note(&state).as_deref(), Some(",enter,load|,enter,load"));
}

#[test]
fn on_window_close_can_keep_the_file_open() {
    let options = FileOptions {
        window_close: Some(145),
        last_window_close: Some(146),
        ..FileOptions::default()
    };
    let scripts = vec![
        script(
            145,
            "Keep open",
            vec![
                set_field("Note", "\"closing\""),
                step(75),
                exit_script("False"),
            ],
        ),
        script(146, "Last", vec![set_field("Note", "\"last\""), step(75)]),
    ];
    let (mut app, state) = open_with(State {
        scripts: extra(scripts),
        file_options: Some(options),
        ..Default::default()
    });
    common::open_menu(&mut app, "File");
    common::click(&mut app, "Close");
    wait_idle(&mut app);
    common::wait_until(&mut app, "OnWindowClose's commit", |_| {
        note(&state).as_deref() == Some("closing")
    });
    app.run_steps(4);
    assert!(app.state().is_open(), "the close was cancelled");
    app.get_by_label("ENTRY LAYOUT");
}

/// A PDF of one 300 x 200 page per marker, each showing its marker in
/// Helvetica, the pages sharing one font object.
fn marked_pdf(markers: &[&str]) -> Vec<u8> {
    titled_pdf(markers, None)
}

/// [`marked_pdf`] with `title` in its document information.
fn titled_pdf(markers: &[&str], title: Option<&str>) -> Vec<u8> {
    use pdf_writer::{Finish, Name, Pdf, Rect, Ref, TextStr};
    let mut pdf = Pdf::new();
    if let Some(title) = title {
        pdf.document_info(Ref::new(99)).title(TextStr(title));
    }
    let count = markers.len() as i32;
    pdf.catalog(Ref::new(1)).pages(Ref::new(2));
    pdf.pages(Ref::new(2))
        .kids((0..count).map(|i| Ref::new(4 + 2 * i)))
        .count(count);
    pdf.indirect(Ref::new(3))
        .dict()
        .pair(Name(b"Type"), Name(b"Font"))
        .pair(Name(b"Subtype"), Name(b"Type1"))
        .pair(Name(b"BaseFont"), Name(b"Helvetica"));
    for (i, marker) in markers.iter().enumerate() {
        let (page, content) = (Ref::new(4 + 2 * i as i32), Ref::new(5 + 2 * i as i32));
        let mut dict = pdf.indirect(page).dict();
        dict.pair(Name(b"Type"), Name(b"Page"));
        dict.pair(Name(b"Parent"), Ref::new(2));
        dict.pair(Name(b"MediaBox"), Rect::new(0.0, 0.0, 300.0, 200.0));
        dict.pair(Name(b"Contents"), content);
        dict.insert(Name(b"Resources"))
            .dict()
            .insert(Name(b"Font"))
            .dict()
            .pair(Name(b"F1"), Ref::new(3));
        dict.finish();
        let text = format!("BT /F1 24 Tf 20 20 Td ({marker}) Tj ET");
        pdf.stream(content, text.as_bytes());
    }
    pdf.finish()
}

/// The MBS plug-in subset's DynaPDF commands (docs/plugins.md) through the
/// desktop's script host, its file layer and the PDF worker: a document of
/// page 3 and page 1 of a PDF read from a file, then every page of one in
/// container data, saved to the file `DynaPDF.OpenOutputFile` named, each
/// command answering as the plug-in documents. A released document, a file
/// that is no PDF, a missing file and a document without pages answer
/// errors.
#[test]
fn dynapdf_assembles_selected_and_whole_pdfs_into_its_output_file() {
    om_data::calc::functions::pdf_worker::set_default(
        env!("CARGO_BIN_EXE_om-gui-pdf-worker").into(),
    );
    let dir = tempfile::tempdir().unwrap();
    let path = |name: &str| dir.path().join(name).to_str().unwrap().to_owned();
    std::fs::write(path("a.pdf"), marked_pdf(&["A1", "A2", "A3"])).unwrap();
    std::fs::write(path("bad.pdf"), b"not a PDF").unwrap();
    let b: String = marked_pdf(&["B1"])
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect();
    let mut names = names();
    names.add_plugin_function(
        om_data::formula::PluginFunctionId {
            plugin: om_data::runtime::plugins::MBS_PLUGIN,
            function: om_data::runtime::plugins::MBS_FUNCTION,
        },
        "MBS",
        1,
        None,
    );
    let mbs = |name: &str, source: &str| {
        let compiled = compile(source, &Context::new(&names)).expect("formula compiles");
        let mut s = step(141);
        s.set_text(0, Some(name));
        s.set_calculation(
            1,
            Some(&Calculation::new(
                Some(formatted::encode(&compiled.tokens)),
                polish::encode(&compiled.expr),
            )),
        );
        s
    };
    let steps = vec![
        capture_errors(),
        mbs("$pdf", r#"MBS ( "DynaPDF.New" )"#),
        mbs(
            "$out",
            &format!(r#"MBS ( "DynaPDF.OpenOutputFile" ; $pdf ; {:?} )"#, path("out.pdf")),
        ),
        mbs(
            "$o1",
            &format!(r#"MBS ( "DynaPDF.OpenPDFFromFile" ; $pdf ; {:?} )"#, path("a.pdf")),
        ),
        mbs("$i1", r#"MBS ( "DynaPDF.ImportPDFPage" ; $pdf ; 3 )"#),
        mbs("$i2", r#"MBS ( "DynaPDF.ImportPDFPage" ; $pdf ; 1 )"#),
        mbs(
            "$o2",
            &format!(
                r#"MBS ( "DynaPDF.OpenPDFFromContainer" ; $pdf ; HexDecode ( "{b}" ; "b.pdf" ) )"#
            ),
        ),
        mbs("$n1", r#"MBS ( "DynaPDF.GetPageCount" ; $pdf )"#),
        mbs("$i3", r#"MBS ( "DynaPDF.ImportPDFFile" ; $pdf ; $n1 + 1 )"#),
        mbs(
            "$s",
            &format!(r#"MBS ( "DynaPDF.Save" ; $pdf ; {:?} )"#, path("out.pdf")),
        ),
        mbs("$r", r#"MBS ( "DynaPDF.Release" ; $pdf )"#),
        mbs(
            "$d",
            &format!(r#"MBS ( "Files.Delete" ; {:?} )"#, path("a.pdf")),
        ),
        mbs("$e1", r#"MBS ( "DynaPDF.GetPageCount" ; $pdf )"#),
        mbs("$pdf2", r#"MBS ( "DynaPDF.New" )"#),
        mbs(
            "$e2",
            &format!(r#"MBS ( "DynaPDF.OpenPDFFromFile" ; $pdf2 ; {:?} )"#, path("bad.pdf")),
        ),
        mbs(
            "$e3",
            &format!(r#"MBS ( "DynaPDF.OpenPDFFromFile" ; $pdf2 ; {:?} )"#, path("none.pdf")),
        ),
        mbs("$e4", r#"MBS ( "DynaPDF.Save" ; $pdf2 )"#),
        mbs("$e5", r#"MBS ( "DynaPDF.Release" ; $pdf2 )"#),
        set_field(
            "Note",
            "( $pdf >= 20000 ) & ( $pdf2 = $pdf + 1 ) & \"|\" & $out & \"|\" & $o1 & \"|\" & $i1 & \"|\" & $i2 & \"|\" & $o2 & \"|\" & $n1 & \"|\" & $i3 & \"|\" & $s & \"|\" & $r & \"|\" & $d & \"|\" & $e1 & \"|\" & $e2 & \"|\" & $e3 & \"|\" & $e4 & \"|\" & $e5",
        ),
        step(75),
    ];
    let (mut app, state) = open_with(State {
        scripts: ScriptCatalog {
            scripts: vec![script(95, "DynaPDF", steps)],
            ..Default::default()
        },
        ..Default::default()
    });
    run(&mut app, "DynaPDF");
    wait_idle(&mut app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(
        notes(&state)[0].as_deref(),
        Some(
            "11|OK|0|OK|OK|0|2|3|OK|OK|OK|[MBS] Invalid PDF reference.\
             |[MBS] The file is not a PDF OpenMaker can read.|[MBS] File not found.\
             |[MBS] The PDF document has no pages.|OK"
        )
    );
    // The source the script deleted after saving.
    assert!(!std::path::Path::new(&path("a.pdf")).exists());
    let out = std::fs::read(path("out.pdf")).unwrap();
    let worker = om_data::calc::functions::pdf_worker::text(&out).unwrap();
    assert_eq!(worker.as_deref(), Some("A3\rA1\rB1"));
}

/// The DynaPDF text subset (docs/plugins.md) through the desktop's script
/// host, its report fonts and the PDF worker. On page 1 of an imported PDF,
/// `WriteFText` writes into the text rectangle in the fonts, sizes and
/// colors its tags name, Arial and Helvetica set in Liberation Sans, and
/// `SetBBox` widens the page; page 2 is as imported. A fill color past the
/// documented 0-1 range, EditPage while a page is open, a font OpenMaker
/// does not have, and formatted text with no text rectangle are refused.
#[test]
fn dynapdf_writes_formatted_text_on_an_imported_page() {
    om_data::calc::functions::pdf_worker::set_default(
        env!("CARGO_BIN_EXE_om-gui-pdf-worker").into(),
    );
    let dir = tempfile::tempdir().unwrap();
    let path = |name: &str| dir.path().join(name).to_str().unwrap().to_owned();
    std::fs::write(path("a.pdf"), marked_pdf(&["A1", "A2"])).unwrap();
    let mut names = names();
    names.add_plugin_function(
        om_data::formula::PluginFunctionId {
            plugin: om_data::runtime::plugins::MBS_PLUGIN,
            function: om_data::runtime::plugins::MBS_FUNCTION,
        },
        "MBS",
        1,
        None,
    );
    let mbs = |name: &str, source: &str| {
        let compiled = compile(source, &Context::new(&names)).expect("formula compiles");
        let mut s = step(141);
        s.set_text(0, Some(name));
        s.set_calculation(
            1,
            Some(&Calculation::new(
                Some(formatted::encode(&compiled.tokens)),
                polish::encode(&compiled.expr),
            )),
        );
        s
    };
    // A FileMaker text literal keeps `\\` as one backslash.
    let tagged = r#""\\FS[11]\\FT[Helvetica-Bold]\\FC[16711680]Label: \\FC[0]R-1¶\\FS[7]\\FT[Helvetica]Line two""#;
    let steps = vec![
        capture_errors(),
        mbs("$pdf", r#"MBS ( "DynaPDF.New" )"#),
        mbs(
            "$o",
            &format!(r#"MBS ( "DynaPDF.OpenPDFFromFile" ; $pdf ; {:?} )"#, path("a.pdf")),
        ),
        mbs("$i", r#"MBS ( "DynaPDF.ImportPDFFile" ; $pdf )"#),
        mbs("$find_closed", r#"MBS ( "DynaPDF.FindPattern" ; $pdf ; "A1" )"#),
        mbs("$e", r#"MBS ( "DynaPDF.EditPage" ; $pdf ; 1 )"#),
        mbs("$found", r#"MBS ( "DynaPDF.FindPattern" ; $pdf ; "A1" )"#),
        mbs("$highlight", r#"MBS ( "DynaPDF.HighlightPattern" ; $pdf ; "a1" ; 65535 ; 1 )"#),
        mbs("$highlight_none", r#"MBS ( "DynaPDF.HighlightPattern" ; $pdf ; "Absent" )"#),
        mbs("$flat_bad", r#"MBS ( "DynaPDF.FlattenAnnots" ; $pdf ; 16 )"#),
        mbs("$flat0", r#"MBS ( "DynaPDF.FlattenAnnots" ; $pdf )"#),
        mbs("$pw", r#"MBS ( "DynaPDF.GetPageWidth" ; $pdf )"#),
        mbs("$ph", r#"MBS ( "DynaPDF.GetPageHeight" ; $pdf )"#),
        mbs(
            "$f",
            r#"MBS ( "DynaPDF.SetFont" ; $pdf ; "Arial" ; 0 ; 12 ; 1 ; 1252 )"#,
        ),
        mbs("$c", r#"MBS ( "DynaPDF.SetFillColor" ; $pdf ; 2 ; 0.5 ; 0 )"#),
        mbs("$u", &format!(r#"MBS ( "DynaPDF.WriteFText" ; $pdf ; "left" ; {tagged} )"#)),
        mbs("$t", r#"MBS ( "DynaPDF.SetTextRect" ; $pdf ; 20 ; 180 ; 260 ; 150 )"#),
        mbs("$w", &format!(r#"MBS ( "DynaPDF.WriteFText" ; $pdf ; "left" ; {tagged} )"#)),
        mbs("$find_edited", r#"MBS ( "DynaPDF.FindPattern" ; $pdf ; "Label" )"#),
        mbs("$highlight_edited", r#"MBS ( "DynaPDF.HighlightPattern" ; $pdf ; "Label" )"#),
        mbs("$b", r#"MBS ( "DynaPDF.SetBBox" ; $pdf ; "Media" ; 0 ; 0 ; 470 ; 200 )"#),
        // Page 1's boxes as SetBBox left them; it has no trim box.
        mbs("$bb", r#"MBS ( "DynaPDF.GetPageBBox" ; $pdf ; 1 ; "MediaBox" )"#),
        mbs("$bt", r#"MBS ( "DynaPDF.GetPageBBox" ; $pdf ; 1 ; "TrimBox" )"#),
        mbs("$pw2", r#"MBS ( "DynaPDF.GetPageWidth" ; $pdf )"#),
        mbs("$x", r#"MBS ( "DynaPDF.EditPage" ; $pdf ; 2 )"#),
        mbs("$n", r#"MBS ( "DynaPDF.SetFont" ; $pdf ; "Times" ; 0 ; 9 )"#),
        mbs("$d", r#"MBS ( "DynaPDF.EndPage" ; $pdf )"#),
        // Text too tall for its rectangle is refused when written, and
        // what was written still saves.
        mbs("$p", r#"MBS ( "DynaPDF.EditPage" ; $pdf ; 2 )"#),
        mbs(
            "$q",
            r#"MBS ( "DynaPDF.WriteFTextEx" ; $pdf ; 20 ; 100 ; 200 ; 1 ; "left" ; "Too tall" )"#,
        ),
        mbs(
            "$v",
            r#"MBS ( "DynaPDF.WriteFTextEx" ; $pdf ; 20 ; 100 ; 200 ; -1 ; "Center" ; "Page two" )"#,
        ),
        // Form fields on page 2, in the colors set before them. A check box
        // of a name already made joins its group; a field of that name and
        // another type, a box of the checked value left unchecked, a
        // parent, and transparent text are refused.
        mbs(
            "$fb",
            r#"MBS ( "DynaPDF.SetFieldBorderColor" ; $pdf ; MBS ( "DynaPDF.RGB" ; 255 ; 0 ; 0 ) )"#,
        ),
        mbs(
            "$fk",
            r#"MBS ( "DynaPDF.SetFieldBackColor" ; $pdf ; 4294967281 )"#,
        ),
        mbs(
            "$ft",
            r#"MBS ( "DynaPDF.SetFieldTextColor" ; $pdf ; 4294967281 )"#,
        ),
        mbs(
            "$f1",
            r#"MBS ( "DynaPDF.CreateTextField" ; $pdf ; "Notes" ; -1 ; 0 ; 0 ; 20 ; 20 ; 150 ; 18 )"#,
        ),
        mbs(
            "$f2",
            r#"MBS ( "DynaPDF.CreateCheckBox" ; $pdf ; "Approved" ; "Yes" ; 1 ; -1 ; 180 ; 20 ; 14 ; 14 )"#,
        ),
        mbs(
            "$f3",
            r#"MBS ( "DynaPDF.CreateSigField" ; $pdf ; "Signature" ; -1 ; 20 ; 140 ; 150 ; 30 )"#,
        ),
        mbs(
            "$f4",
            r#"MBS ( "DynaPDF.CreateCheckBox" ; $pdf ; "Approved" ; "Changes" ; 0 ; -1 ; 200 ; 20 ; 14 ; 14 )"#,
        ),
        mbs(
            "$f4t",
            r#"MBS ( "DynaPDF.CreateTextField" ; $pdf ; "Approved" ; -1 ; 0 ; 0 ; 20 ; 60 ; 150 ; 18 )"#,
        ),
        mbs(
            "$f4c",
            r#"MBS ( "DynaPDF.CreateCheckBox" ; $pdf ; "Approved" ; "Yes" ; 0 ; -1 ; 260 ; 20 ; 14 ; 14 )"#,
        ),
        mbs(
            "$f5",
            r#"MBS ( "DynaPDF.CreateTextField" ; $pdf ; "Group" ; 3 ; 0 ; 0 ; 20 ; 60 ; 150 ; 18 )"#,
        ),
        // Nor a name with a period, the off state's export value, or a
        // rectangle whose corners are one point.
        mbs(
            "$f6",
            r#"MBS ( "DynaPDF.CreateTextField" ; $pdf ; "a.b" ; -1 ; 0 ; 0 ; 20 ; 60 ; 150 ; 18 )"#,
        ),
        mbs(
            "$f7",
            r#"MBS ( "DynaPDF.CreateCheckBox" ; $pdf ; "Off box" ; "Off" ; 1 ; -1 ; 220 ; 20 ; 14 ; 14 )"#,
        ),
        mbs(
            "$f8",
            r#"MBS ( "DynaPDF.CreateSigField" ; $pdf ; "Far" ; -1 ; 100000000000000000000 ; 0 ; 1 ; 14 )"#,
        ),
        // A 1-point font makes the check's size automatic; a 7-point
        // border leaves a 14-point box no room for it.
        mbs("$g1", r#"MBS ( "DynaPDF.SetFont" ; $pdf ; "Arial" ; 0 ; 1 )"#),
        mbs("$g2", r#"MBS ( "DynaPDF.SetLineWidth" ; $pdf ; 7 )"#),
        mbs(
            "$g3",
            r#"MBS ( "DynaPDF.CreateCheckBox" ; $pdf ; "Tight" ; "Yes" ; 1 ; -1 ; 240 ; 20 ; 14 ; 14 )"#,
        ),
        // A button with its caption, running a JavaScript action when
        // clicked; a second action on it, another object type, a second
        // button of the name and a caption taller than its button are
        // refused.
        mbs("$b0", r#"MBS ( "DynaPDF.SetFont" ; $pdf ; "Helvetica" ; 0 ; 9 )"#),
        mbs("$b1", r#"MBS ( "DynaPDF.SetLineWidth" ; $pdf ; 1 )"#),
        mbs(
            "$b2",
            r#"MBS ( "DynaPDF.CreateButton" ; $pdf ; "Next" ; "Next" ; -1 ; 200 ; 60 ; 40 ; 14 )"#,
        ),
        mbs("$b3", r#"MBS ( "DynaPDF.CreateJSAction" ; $pdf ; "this.pageNum++;" )"#),
        mbs(
            "$b4",
            r#"MBS ( "DynaPDF.AddActionToObj" ; $pdf ; "Field" ; "OnMouseUp" ; $b3 ; $b2 )"#,
        ),
        mbs(
            "$b5",
            r#"MBS ( "DynaPDF.AddActionToObj" ; $pdf ; "Field" ; "OnMouseUp" ; $b3 ; $b2 )"#,
        ),
        mbs(
            "$b6",
            r#"MBS ( "DynaPDF.AddActionToObj" ; $pdf ; "Page" ; "OnMouseUp" ; $b3 ; 1 )"#,
        ),
        mbs(
            "$b7",
            r#"MBS ( "DynaPDF.CreateButton" ; $pdf ; "Next" ; "Again" ; -1 ; 200 ; 80 ; 40 ; 14 )"#,
        ),
        mbs(
            "$b8",
            r#"MBS ( "DynaPDF.CreateButton" ; $pdf ; "Tiny" ; "Too tall" ; -1 ; 20 ; 100 ; 60 ; 6 )"#,
        ),
        mbs(
            "$b9",
            r#"MBS ( "DynaPDF.CreateButton" ; $pdf ; "Narrow" ; "WWWWWWWWWW" ; -1 ; 20 ; 120 ; 20 ; 30 )"#,
        ),
        // A web link on the page; a URL beyond 7-bit ASCII is refused.
        mbs(
            "$l1",
            r#"MBS ( "DynaPDF.WebLink" ; $pdf ; 20 ; 170 ; 100 ; 20 ; "https://example.com/page" )"#,
        ),
        mbs(
            "$l2",
            r#"MBS ( "DynaPDF.WebLink" ; $pdf ; 20 ; 170 ; 100 ; 20 ; "https://example.com/prüfung" )"#,
        ),
        mbs("$flat_keep", r#"MBS ( "DynaPDF.FlattenAnnots" ; $pdf ; "VisibleMarkupAnnots" )"#),
        mbs(
            "$out",
            &format!(r#"MBS ( "DynaPDF.OpenOutputFile" ; $pdf ; {:?} )"#, path("out.pdf")),
        ),
        mbs("$s", r#"MBS ( "DynaPDF.Save" ; $pdf )"#),
        mbs("$flat_default", r#"MBS ( "DynaPDF.FlattenAnnots" ; $pdf ; "None" )"#),
        mbs("$flat_out", &format!(r#"MBS ( "DynaPDF.OpenOutputFile" ; $pdf ; {:?} )"#, path("flat-default.pdf"))),
        mbs("$flat_s", r#"MBS ( "DynaPDF.Save" ; $pdf )"#),
        mbs("$r", r#"MBS ( "DynaPDF.Release" ; $pdf )"#),
        // Keep an editable-highlight output, then write/search/highlight
        // another slug and flatten with default flags before saving it.
        mbs("$searchpdf", r#"MBS ( "DynaPDF.New" )"#),
        mbs("$search_open", &format!(r#"MBS ( "DynaPDF.OpenPDFFromFile" ; $searchpdf ; {:?} )"#, path("a.pdf"))),
        mbs("$search_import", r#"MBS ( "DynaPDF.ImportPDFFile" ; $searchpdf )"#),
        mbs("$search_edit", r#"MBS ( "DynaPDF.EditPage" ; $searchpdf ; 1 )"#),
        mbs("$search_font", r#"MBS ( "DynaPDF.SetFont" ; $searchpdf ; "Arial" ; 0 ; 12 )"#),
        mbs("$search_rect", r#"MBS ( "DynaPDF.SetTextRect" ; $searchpdf ; 20 ; 180 ; 260 ; 150 )"#),
        mbs("$search_write", r#"MBS ( "DynaPDF.WriteFText" ; $searchpdf ; "left" ; "Pending" )"#),
        mbs("$search_find", r#"MBS ( "DynaPDF.FindPattern" ; $searchpdf ; "Pending" )"#),
        mbs("$search_highlight", r#"MBS ( "DynaPDF.HighlightPattern" ; $searchpdf ; "Pending" )"#),
        mbs("$search_out", &format!(r#"MBS ( "DynaPDF.OpenOutputFile" ; $searchpdf ; {:?} )"#, path("search.pdf"))),
        mbs("$search_save", r#"MBS ( "DynaPDF.Save" ; $searchpdf )"#),
        mbs("$later_edit", r#"MBS ( "DynaPDF.EditPage" ; $searchpdf ; 1 )"#),
        mbs("$later_write", r#"MBS ( "DynaPDF.WriteFTextEx" ; $searchpdf ; 20 ; 140 ; 260 ; 100 ; "left" ; "Later" )"#),
        mbs("$later_find", r#"MBS ( "DynaPDF.FindPattern" ; $searchpdf ; "Later" )"#),
        mbs("$later_highlight", r#"MBS ( "DynaPDF.HighlightPattern" ; $searchpdf ; "Later" )"#),
        mbs("$later_flat", r#"MBS ( "DynaPDF.FlattenAnnots" ; $searchpdf )"#),
        mbs("$pending_again", r#"MBS ( "DynaPDF.FindPattern" ; $searchpdf ; "Pending" )"#),
        // Text after the boundary overlays baked appearances. The original
        // import source is still available after the content snapshot.
        mbs("$after_write", r#"MBS ( "DynaPDF.WriteFTextEx" ; $searchpdf ; 20 ; 100 ; 260 ; 60 ; "left" ; "After" )"#),
        mbs("$later_out", &format!(r#"MBS ( "DynaPDF.OpenOutputFile" ; $searchpdf ; {:?} )"#, path("flatten-pending.pdf"))),
        mbs("$later_save", r#"MBS ( "DynaPDF.Save" ; $searchpdf )"#),
        mbs("$original_import", r#"MBS ( "DynaPDF.ImportPDFPage" ; $searchpdf ; 1 )"#),
        mbs("$original_out", &format!(r#"MBS ( "DynaPDF.OpenOutputFile" ; $searchpdf ; {:?} )"#, path("flatten-import.pdf"))),
        mbs("$original_save", r#"MBS ( "DynaPDF.Save" ; $searchpdf )"#),
        mbs("$search_release", r#"MBS ( "DynaPDF.Release" ; $searchpdf )"#),
        set_field(
            "Note",
            "$e & \"|\" & $f & \"|\" & $c & \"|\" & $u & \"|\" & $t & \"|\" & $w & \"|\" & $b & \"|\" & $x & \"|\" & $n & \"|\" & $d & \"|\" & $p & \"|\" & $q & \"|\" & $v & \"|\" & $s & \"|\" & $pw & \"|\" & $ph & \"|\" & $bb & \"|\" & $bt & \"|\" & $pw2 & \"|\" & $fb & \"|\" & $fk & \"|\" & $ft & \"|\" & $f1 & \"|\" & $f2 & \"|\" & $f3 & \"|\" & $f4 & \"|\" & $f4t & \"|\" & $f4c & \"|\" & $f5 & \"|\" & $f6 & \"|\" & $f7 & \"|\" & $f8 & \"|\" & $g3 & \"|\" & $b2 & \"|\" & $b3 & \"|\" & $b4 & \"|\" & $b5 & \"|\" & $b6 & \"|\" & $b7 & \"|\" & $b8 & \"|\" & $b9 & \"|\" & $l1 & \"|\" & $l2 & \"|\" & $find_closed & \"|\" & $found & \"|\" & $find_edited & \"|\" & $highlight & \"|\" & $highlight_none & \"|\" & $highlight_edited & \"|\" & $flat_bad & \"|\" & $flat0 & \"|\" & $flat_keep & \"|\" & $flat_default & \"|\" & $flat_s & \"|\" & $search_find & \"|\" & $search_highlight & \"|\" & $search_save & \"|\" & $later_find & \"|\" & $later_highlight & \"|\" & $later_flat & \"|\" & $pending_again & \"|\" & $after_write & \"|\" & $later_save & \"|\" & $original_import & \"|\" & $original_save",
        ),
        step(75),
    ];
    let (mut app, state) = open_with(State {
        scripts: ScriptCatalog {
            scripts: vec![script(96, "DynaPDF text", steps)],
            ..Default::default()
        },
        ..Default::default()
    });
    run(&mut app, "DynaPDF text");
    wait_idle(&mut app);
    assert!(app.state().error().is_none(), "{:?}", app.state().error());
    assert_eq!(
        notes(&state)[0].as_deref(),
        Some(
            "OK|0|[MBS] OpenMaker sets an RGB fill color of three components from 0 to 1.\
             |[MBS] OpenMaker writes formatted text only after DynaPDF.SetTextRect.|OK|OK|OK\
             |[MBS] A page is open; OpenMaker opens another only after DynaPDF.EndPage.\
             |[MBS] OpenMaker has no font Times.|OK|OK\
             |[MBS] The text does not fit its rectangle; OpenMaker does not break pages.|OK|OK\
             |300|200|0.000000\r200.000000\r470.000000\r0.000000\
             |[MBS] The page has no such box.|470\
             |OK|OK|[MBS] Invalid color.|0|1|2|3\
             |[MBS] A field group holds fields of one type.\
             |[MBS] OpenMaker checks one export value of a field group, on every box of it.\
             |[MBS] OpenMaker creates only fields without a parent (-1).\
             |[MBS] Invalid field name.|[MBS] Invalid export value.\
             |[MBS] Invalid field rectangle.\
             |[MBS] The check box has no room inside its border for its check.\
             |4|0|OK|[MBS] OpenMaker attaches one action to a field's OnMouseUp.\
             |[MBS] OpenMaker attaches actions only to a field's OnMouseUp.\
             |[MBS] OpenMaker creates one button of a name.\
             |[MBS] The caption does not fit its button.\
             |[MBS] The caption does not fit its button.\
             |0|[MBS] OpenMaker links to a URL of printable 7-bit ASCII only.\
             |[MBS] No page is open.|1\
             |1\
             |1|0|1\
             |[MBS] OpenMaker flattens annotations with flags 0 through 3 only.|0|1|0|OK|1|1|OK|1|1|0|1|OK|OK|OK|OK"
        )
    );
    let search_out = std::fs::read(path("search.pdf")).unwrap();
    assert_eq!(
        om_data::calc::functions::pdf_worker::text(&search_out)
            .unwrap()
            .as_deref(),
        Some("A1\rPending\rA2")
    );
    let search_pdf = hayro::hayro_interpret::hayro_syntax::Pdf::new(search_out.clone()).unwrap();
    use hayro::hayro_interpret::hayro_syntax::object::{Array as SearchArray, Dict as SearchDict};
    let marks = search_pdf.pages()[0]
        .raw()
        .get::<SearchArray<'_>>(b"Annots")
        .unwrap();
    let mark = marks.iter::<SearchDict<'_>>().next().unwrap();
    let rect = mark
        .get::<SearchArray<'_>>(b"Rect")
        .unwrap()
        .iter::<f64>()
        .collect::<Vec<_>>();
    assert!(
        rect[0] >= 20.0 && rect[1] > 100.0 && rect[2] < 140.0,
        "new text bounds: {rect:?}"
    );
    if let Some(kept) = std::env::var_os("OM_PDF_ARTIFACT_DIR") {
        std::fs::copy(
            path("a.pdf"),
            std::path::Path::new(&kept).join("dynapdf-search-before.pdf"),
        )
        .unwrap();
        std::fs::write(
            std::path::Path::new(&kept).join("dynapdf-search.pdf"),
            &search_out,
        )
        .unwrap();
    }
    let flattened = std::fs::read(path("flatten-pending.pdf")).unwrap();
    assert_eq!(
        om_data::calc::functions::pdf_worker::text(&flattened)
            .unwrap()
            .as_deref(),
        Some("A1\rPending\rLater\rAfter\rA2"),
        "snapshot content is neither lost nor duplicated by a later overlay"
    );
    let parsed_flat = hayro::hayro_interpret::hayro_syntax::Pdf::new(flattened.clone()).unwrap();
    assert!(parsed_flat.pages()[0]
        .raw()
        .get::<SearchArray<'_>>(b"Annots")
        .is_none());
    let flat_raw = String::from_utf8_lossy(&flattened);
    assert!(!flat_raw.contains("/Subtype /Highlight") && flat_raw.contains("/BM /Multiply"));
    let imported = std::fs::read(path("flatten-import.pdf")).unwrap();
    assert_eq!(
        om_data::calc::functions::pdf_worker::text(&imported)
            .unwrap()
            .as_deref(),
        Some("A1\rPending\rLater\rAfter\rA2\rA1"),
        "a later import still reads the original source"
    );
    if let Some(kept) = std::env::var_os("OM_PDF_ARTIFACT_DIR") {
        std::fs::write(
            std::path::Path::new(&kept).join("dynapdf-flatten-pending.pdf"),
            &flattened,
        )
        .unwrap();
    }
    let out = std::fs::read(path("out.pdf")).unwrap();
    if let Some(kept) = std::env::var_os("OM_PDF_ARTIFACT_DIR") {
        std::fs::write(std::path::Path::new(&kept).join("dynapdf-text.pdf"), &out).unwrap();
    }
    let text = om_data::calc::functions::pdf_worker::text(&out).unwrap();
    assert_eq!(
        text.as_deref(),
        // The button's caption shows in its appearance.
        Some("A1\rLabel: R-1\rLine two\rA2\rPage two\rNext")
    );
    let parsed = hayro::hayro_interpret::hayro_syntax::Pdf::new(out.clone()).unwrap();
    let media = parsed.pages()[0].media_box();
    assert_eq!(
        [media.x0, media.y0, media.x1, media.y1],
        [0.0, 0.0, 470.0, 200.0]
    );
    // Page 1 carries baked highlight content; markup-only leaves page 2's
    // widgets and URI link interactive.
    let raw_pdf = String::from_utf8_lossy(&out);
    assert!(!raw_pdf.contains("/Subtype /Highlight") && raw_pdf.contains("/BM /Multiply"));
    let default_flat = std::fs::read(path("flat-default.pdf")).unwrap();
    assert!(!String::from_utf8_lossy(&default_flat).contains("/Subtype /Link"));
    let default_parsed = hayro::hayro_interpret::hayro_syntax::Pdf::new(default_flat).unwrap();
    assert_eq!(
        default_parsed.pages()[1]
            .raw()
            .get::<hayro::hayro_interpret::hayro_syntax::object::Array<'_>>(b"Annots")
            .unwrap()
            .raw_iter()
            .count(),
        5,
        "default flattening keeps all five widgets"
    );
    // The fields: in the interactive form (the two Approved boxes one
    // field), and widgets of page 2.
    use hayro::hayro_interpret::hayro_syntax::object::{Array, Dict};
    let catalog: Dict<'_> = parsed.xref().get(parsed.xref().root_id()).unwrap();
    let fields = catalog
        .get::<Dict<'_>>(b"AcroForm")
        .and_then(|form| form.get::<Array<'_>>(b"Fields"))
        .map_or(0, |fields| fields.iter::<Dict<'_>>().count());
    assert_eq!(fields, 4);
    let widgets = parsed.pages()[1]
        .raw()
        .get::<Array<'_>>(b"Annots")
        .map_or(0, |annots| annots.iter::<Dict<'_>>().count());
    assert_eq!(widgets, 6, "five widgets and one URI link");
    // The button runs its JavaScript, kept as text.
    let raw_text = String::from_utf8_lossy(&out);
    assert!(raw_text.contains("/S /JavaScript"));
    assert!(raw_text.contains("/Subtype /Link") && raw_text.contains("/S /URI"));
    let raw = String::from_utf8_lossy(&out);
    assert!(
        raw.contains("/LiberationSans-Bold"),
        "the bold face is embedded"
    );
    assert!(
        raw.contains("/LiberationSans "),
        "the regular face is embedded"
    );
}

/// A provider panic while a script runs (#1247), here in a second window
/// the script opened and froze, as it commits a field value it set. The
/// worker survives it and ends the script as Halt Script would: the window
/// shows what the script left in it (the value, not saved) and says the
/// script stopped, takes input again, and its next script runs and is
/// answered. Before, the message went to no window, the app kept the
/// script running (every window's actions and scripts waited behind it),
/// and the worker forgot every window but the first, answering none of
/// their requests: the second window stopped responding while nothing ran.
/// Its first fix ended the script without telling the frozen window what
/// it held: the next script started from the window as the user last saw
/// it, and the value was lost. So it was when the panic came as a script
/// ended, after its last step: no script was running any more, and that
/// end was never told.
#[test]
fn a_script_stopped_by_a_provider_panic_frees_its_window() {
    let mut detail = step(122);
    detail.set_calculation(0, Some(&calc("\"Detail\"")));
    detail.set_member_key(StoredKey::new(&[1, OTHER as u8]));
    let (mut app, state) = open_with(State {
        scripts: ScriptCatalog {
            scripts: vec![
                script(
                    900,
                    "Detail then commit",
                    vec![detail, step(79), set_field("Note", "\"kept\""), step(75)],
                ),
                script(
                    901,
                    "Freeze then set",
                    vec![step(79), set_field("Note", "\"again\"")],
                ),
            ],
            ..Default::default()
        },
        ..Default::default()
    });
    state.lock().unwrap().panic_on_update = true;
    run(&mut app, "Detail then commit");
    common::wait_until(&mut app, "the stopped script's message", |app| {
        app.state()
            .error()
            .is_some_and(|e| e.contains("stopped unexpectedly"))
    });
    common::click(&mut app, "OK");
    wait_idle(&mut app);
    // What the frozen window held when the script stopped is shown, not
    // dropped for the window as the user last saw it.
    common::wait_until(&mut app, "the script's unsaved value", |app| {
        app.query_all_by_role_and_label(egui::accesskit::Role::TextInput, "Note")
            .last()
            .and_then(|f| f.value())
            .as_deref()
            == Some("kept")
    });
    assert_eq!(note(&state), None, "the panicked commit saved nothing");
    // The second window, in front, still runs scripts and is answered
    // (its menu bar is the last of the two windows').
    assert_eq!(app.state().window_name(), "Detail");
    app.get_all_by_role_and_label(egui::accesskit::Role::Button, "Scripts")
        .last()
        .unwrap()
        .click();
    app.run_steps(2);
    common::click(&mut app, "Missing layout");
    common::wait_until(&mut app, "the second window's layout alert", |app| {
        app.state().error().is_some_and(|e| e.contains("105"))
    });
    common::click(&mut app, "OK");
    wait_idle(&mut app);
    // A panic as a script ends, after its last step, and again as the
    // worker tells that end: the frozen window still shows what the script
    // set.
    state.lock().unwrap().panics_on_unsettled = 2;
    app.get_all_by_role_and_label(egui::accesskit::Role::Button, "Scripts")
        .last()
        .unwrap()
        .click();
    app.run_steps(2);
    common::click(&mut app, "Freeze then set");
    common::wait_until(&mut app, "the ended script's message", |app| {
        app.state()
            .error()
            .is_some_and(|e| e.contains("stopped unexpectedly"))
    });
    common::click(&mut app, "OK");
    wait_idle(&mut app);
    common::wait_until(&mut app, "the ended script's unsaved value", |app| {
        app.query_all_by_role_and_label(egui::accesskit::Role::TextInput, "Note")
            .last()
            .and_then(|f| f.value())
            .as_deref()
            == Some("again")
    });
    assert_ne!(note(&state).as_deref(), Some("again"), "nothing saved it");
}

/// Save Records as PDF [Append to existing PDF] (#109) adds the report's
/// pages after the file's, through the PDF worker: to the PDF the step
/// saved just before, and again to the document that append made; with no
/// file yet, it writes a new one. A PDF another program wrote (without
/// OpenMaker's mark, even with only pages), a file that is not a PDF, and
/// one with document information or a marked file with a bad xref refuse
/// with 3 and stay as they were.
#[test]
fn save_records_as_pdf_appends_and_refuses_what_it_would_not_keep() {
    om_data::calc::functions::pdf_worker::set_default(
        env!("CARGO_BIN_EXE_om-gui-pdf-worker").into(),
    );
    use om_data::calc::functions::pdf_worker;
    let dir = tempfile::tempdir().unwrap();
    let path = |name: &str| dir.path().join(name);
    let plain = marked_pdf(&["P1"]);
    std::fs::write(path("plain.pdf"), &plain).unwrap();
    let mut marked = plain.clone();
    marked[b"%PDF-1.7\n%".len()..][..4].copy_from_slice(pdf_worker::MARKER);
    let xref = marked.windows(7).position(|w| w == b"xref\n0 ").unwrap();
    let start = xref
        + marked[xref..]
            .iter()
            .enumerate()
            .filter(|&(_, &b)| b == b'\n')
            .nth(1)
            .unwrap()
            .0
        + 1;
    // Point the catalog's row at object 2. The marker still matches.
    marked.copy_within(start + 40..start + 50, start + 20);
    std::fs::write(path("bad-xref.pdf"), &marked).unwrap();
    std::fs::write(path("bad.pdf"), b"not a PDF").unwrap();
    let titled = titled_pdf(&["T1"], Some("Kept"));
    std::fs::write(path("titled.pdf"), &titled).unwrap();
    let save = |name: &str, append: bool| {
        let mut step = step(144);
        step.value = 1;
        step.flags |= flag::NO_INTERACT | flag::FILE_PATH;
        if append {
            step.flags |= flag::OPTION;
        }
        step.set_slot_child(
            0,
            0x82,
            Some(om_data::text_node(
                &[0x82],
                &format!("file:{}", path(name).display()),
            )),
        );
        step
    };
    let errors = |name: &str| set_variable(name, "Get ( LastError )");
    let steps = vec![
        capture_errors(),
        save("records.pdf", false),
        errors("$a"),
        save("records.pdf", true),
        errors("$b"),
        save("records.pdf", true),
        errors("$g"),
        save("bad-xref.pdf", true),
        errors("$h"),
        save("plain.pdf", true),
        errors("$c"),
        save("new.pdf", true),
        errors("$d"),
        save("bad.pdf", true),
        errors("$e"),
        save("titled.pdf", true),
        errors("$f"),
        set_field(
            "Note",
            "$a & \"|\" & $b & \"|\" & $g & \"|\" & $c & \"|\" & $d & \"|\" & $e & \"|\" & $f & \"|\" & $h",
        ),
        // Commit Records: the script's end does not save the field.
        step(75),
    ];
    let (mut app, state) = open_with(State {
        calc_schema: true,
        rows: rows_of(&[("Ada", "10"), ("Grace", "20"), ("Alan", "30")]),
        scripts: ScriptCatalog {
            scripts: vec![script(96, "Append", steps)],
            ..Default::default()
        },
        ..Default::default()
    });
    run(&mut app, "Append");
    wait_idle(&mut app);
    assert_eq!(notes(&state)[0].as_deref(), Some("0|0|0|3|0|3|3|3"));
    let pages = |name: &str| {
        pdf_worker::info(&std::fs::read(path(name)).unwrap())
            .unwrap()
            .unwrap()
            .pages
    };
    let once = pages("new.pdf");
    assert!(once > 0);
    assert_eq!(pages("records.pdf"), 3 * once);
    assert_eq!(std::fs::read(path("plain.pdf")).unwrap(), plain);
    assert_eq!(std::fs::read(path("bad-xref.pdf")).unwrap(), marked);
    assert_eq!(std::fs::read(path("bad.pdf")).unwrap(), b"not a PDF");
    assert_eq!(std::fs::read(path("titled.pdf")).unwrap(), titled);
}
