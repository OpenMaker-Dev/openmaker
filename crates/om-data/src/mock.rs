//! A mock [`DataProvider`] with sample data, so the GUI can be built and
//! rendered before the protocol-backed provider is wired in.

use std::sync::Arc;

use crate::host::{CalcData, CalcNeed, CalcSchema};
use crate::mock_calc::{self, CONTACT_CARD};
use crate::{
    Connection, DataError, DataProvider, Field, FieldChanges, FieldKind, GeomRect, HostedFile,
    LayoutGeometry, LayoutInfo, ObjectGeom, PartGeom, Record, RecordPage, RenderLayout,
};

/// The id of the mock's "Contact Details" layout, the only one with geometry.
const CONTACT_DETAILS: u32 = 1;

/// A provider that serves a small fixed contacts dataset.
pub struct MockProvider {
    opened: bool,
    database: Option<crate::DatabaseSchema>,
    /// Manage Database's staged copy of the schema while it is open.
    schema_draft: Option<crate::DatabaseSchema>,
    records: Vec<Record>,
    next_record_id: i64,
    /// Every layout shows the one contacts table.
    found: crate::found::MemoryFoundSet,
}

impl Default for MockProvider {
    fn default() -> Self {
        Self {
            opened: false,
            database: None,
            schema_draft: None,
            records: (0..42)
                .map(|i| Record {
                    id: i + 1,
                    values: vec![
                        Some(format!("First{i}")),
                        Some(format!("Last{i}")),
                        Some(format!("user{i}@example.com")),
                        Some("10/28/2026 09:00:00".into()),
                    ],
                })
                .collect(),
            next_record_id: 43,
            found: Default::default(),
        }
    }
}

impl MockProvider {
    pub fn new() -> Self {
        Self::default()
    }

    fn record_fields(&self) -> Vec<Field> {
        match self.database.as_ref() {
            None => fields(),
            Some(schema) => schema
                .table(0x8002)
                .map(|table| {
                    table
                        .fields
                        .iter()
                        .map(|field| Field {
                            name: field.name.clone(),
                            kind: field.data_type,
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// The schema ids of [`Self::record_fields`], in the same order.
    fn field_ids(&self) -> Vec<u64> {
        match self.database.as_ref() {
            None => (1..=fields().len() as u64).collect(),
            Some(schema) => schema
                .table(0x8002)
                .map(|table| table.fields.iter().map(|f| f.id).collect())
                .unwrap_or_default(),
        }
    }

    fn calculation_schema(&self) -> CalcSchema {
        mock_calc::schema(&self.record_fields(), &self.field_ids())
    }

    fn require_open(&self) -> Result<(), DataError> {
        if self.opened {
            Ok(())
        } else {
            Err(DataError::new("not connected"))
        }
    }
}

fn fields() -> Vec<Field> {
    [
        ("First Name", FieldKind::Text),
        ("Last Name", FieldKind::Text),
        ("Email", FieldKind::Text),
        ("Created", FieldKind::Timestamp),
    ]
    .into_iter()
    .map(|(name, kind)| Field {
        name: name.into(),
        kind,
    })
    .collect()
}

fn apply_values(
    record: &mut Record,
    values: FieldChanges,
    fields: &[Field],
) -> Result<(), DataError> {
    for name in values.keys() {
        if !fields.iter().any(|f| f.name == *name) {
            return Err(DataError::new(format!("Unknown field {name:?}.")));
        }
    }
    for (name, value) in values {
        let index = fields
            .iter()
            .position(|f| f.name == name)
            .expect("validated field");
        record.values[index] = value;
    }
    Ok(())
}

impl DataProvider for MockProvider {
    fn database_schema(&mut self) -> Result<crate::DatabaseSchema, DataError> {
        self.require_open()?;
        crate::schema::mock_schema(&mut self.database, None)
    }

    fn begin_schema_edit(&mut self) -> Result<crate::DatabaseSchema, DataError> {
        self.require_open()?;
        let schema = crate::schema::mock_schema(&mut self.database, None)?;
        self.schema_draft = Some(schema.clone());
        Ok(schema)
    }

    /// Stage `change` on the draft Manage Database shows; records keep
    /// their values until the draft is committed.
    fn apply_schema_change(
        &mut self,
        change: crate::SchemaChange,
    ) -> Result<crate::DatabaseSchema, DataError> {
        self.require_open()?;
        if self.schema_draft.is_none() {
            self.begin_schema_edit()?;
        }
        let draft = self.schema_draft.as_mut().expect("draft begun");
        draft.apply_sample_change(change)?;
        Ok(draft.clone())
    }

    fn commit_schema_changes(&mut self) -> Result<crate::DatabaseSchema, DataError> {
        self.require_open()?;
        let before = crate::schema::mock_schema(&mut self.database, None)?;
        let Some(next) = self.schema_draft.take() else {
            return Ok(before);
        };
        let old_fields = before
            .table(0x8002)
            .map(|table| table.fields.as_slice())
            .unwrap_or_default();
        let new_fields = next
            .table(0x8002)
            .map(|table| table.fields.as_slice())
            .unwrap_or_default();
        // Preserve record values by stable field identity when definitions are
        // renamed/reordered; additions start empty and deletions drop values.
        for record in &mut self.records {
            record.values = new_fields
                .iter()
                .map(|field| {
                    old_fields
                        .iter()
                        .position(|old| old.id == field.id)
                        .and_then(|index| record.values.get(index))
                        .cloned()
                        .flatten()
                })
                .collect();
        }
        if next.table(0x8002).is_none() {
            self.records.clear();
        }
        self.database = Some(next.clone());
        Ok(next)
    }

    fn cancel_schema_changes(&mut self) -> Result<(), DataError> {
        self.schema_draft = None;
        Ok(())
    }

    fn can_write_records(&self) -> bool {
        true
    }

    fn create_record(
        &mut self,
        _layout_id: u32,
        values: FieldChanges,
    ) -> Result<Record, DataError> {
        self.require_open()?;
        let fields = self.record_fields();
        let mut record = Record {
            id: self.next_record_id,
            values: vec![None; fields.len()],
        };
        apply_values(&mut record, values, &fields)?;
        self.next_record_id += 1;
        self.records.push(record.clone());
        self.found.add(record.id);
        Ok(record)
    }

    fn update_record(
        &mut self,
        _layout_id: u32,
        record_id: i64,
        values: FieldChanges,
    ) -> Result<Record, DataError> {
        self.require_open()?;
        let fields = self.record_fields();
        let record = self
            .records
            .iter_mut()
            .find(|r| r.id == record_id)
            .ok_or_else(|| DataError::new("The record no longer exists."))?;
        apply_values(record, values, &fields)?;
        Ok(record.clone())
    }

    fn delete_record(&mut self, _layout_id: u32, record_id: i64) -> Result<(), DataError> {
        self.require_open()?;
        let index = self
            .records
            .iter()
            .position(|r| r.id == record_id)
            .ok_or_else(|| DataError::new("The record no longer exists."))?;
        self.records.remove(index);
        Ok(())
    }

    fn is_sample(&self) -> bool {
        true
    }

    fn list_files(&mut self, _host: &str, _port: u16) -> Result<Vec<HostedFile>, DataError> {
        Ok(vec![
            HostedFile {
                name: "test project.fmp12".into(),
            },
            HostedFile {
                name: "Contacts.fmp12".into(),
            },
        ])
    }

    fn open(&mut self, _connection: &Connection) -> Result<(), DataError> {
        self.opened = true;
        Ok(())
    }

    fn layouts(&mut self) -> Result<Vec<LayoutInfo>, DataError> {
        if !self.opened {
            return Err(DataError::new("not connected"));
        }
        Ok(vec![
            LayoutInfo {
                id: CONTACT_DETAILS,
                name: "Contact Details".into(),
            },
            LayoutInfo {
                id: 2,
                name: "Contacts List".into(),
            },
            LayoutInfo {
                id: CONTACT_CARD,
                name: "Contact Card".into(),
            },
        ])
    }

    fn render_layout(&mut self, layout_id: u32) -> Result<Option<RenderLayout>, DataError> {
        self.require_open()?;
        Ok(
            (layout_id == CONTACT_CARD)
                .then(|| mock_calc::contact_card(&self.calculation_schema())),
        )
    }

    fn calc_schema(&mut self) -> Result<Option<Arc<CalcSchema>>, DataError> {
        self.require_open()?;
        Ok(Some(Arc::new(self.calculation_schema())))
    }

    fn calc_data(&mut self, needs: &[CalcNeed]) -> Result<CalcData, DataError> {
        self.require_open()?;
        let schema = self.calculation_schema();
        mock_calc::data(
            &schema,
            &self.record_fields(),
            &self.field_ids(),
            &self.records,
            needs,
        )
    }

    fn records(
        &mut self,
        _layout_id: u32,
        offset: u64,
        limit: u32,
    ) -> Result<RecordPage, DataError> {
        if !self.opened {
            return Err(DataError::new("not connected"));
        }
        Ok(self
            .found
            .page(self.record_fields(), &self.records, offset, limit))
    }

    fn change_found_set(
        &mut self,
        _layout_id: u32,
        change: crate::found::FoundSetChange,
    ) -> Result<crate::found::FoundSet, DataError> {
        self.require_open()?;
        let fields = self.record_fields();
        self.found.apply(&fields, &self.records, change)
    }

    fn record_by_id(&mut self, _layout_id: u32, record_id: i64) -> Result<RecordPage, DataError> {
        self.require_open()?;
        let records: Vec<Record> = self
            .records
            .iter()
            .filter(|r| r.id == record_id)
            .cloned()
            .collect();
        Ok(RecordPage {
            versions: Default::default(),
            fields: self.record_fields(),
            total: records.len() as u64,
            records,
        })
    }

    fn record_position(
        &mut self,
        _layout_id: u32,
        record_id: i64,
    ) -> Result<Option<u64>, DataError> {
        self.require_open()?;
        let fields = self.record_fields();
        Ok(self
            .found
            .select(&fields, &self.records)
            .iter()
            .position(|r| r.id == record_id)
            .map(|i| i as u64))
    }

    fn layout_geometry(&mut self, layout_id: u32) -> Result<Option<LayoutGeometry>, DataError> {
        if !self.opened {
            return Err(DataError::new("not connected"));
        }
        Ok((layout_id == CONTACT_DETAILS).then(contact_details))
    }

    fn close(&mut self) {
        self.opened = false;
    }
}

/// A hand-made detail layout: a title and a button in the header, a panel
/// holding a label and a field per record field in the body, and a footer
/// note.
fn contact_details() -> LayoutGeometry {
    let part = |kind_name: &str, top: f64, height: f64| PartGeom {
        kind_name: kind_name.into(),
        top,
        height,
    };
    let object = |kind_name: &str, top: f64, left: f64, bottom: f64, right: f64| ObjectGeom {
        kind_name: kind_name.into(),
        rect: GeomRect {
            top,
            left,
            bottom,
            right,
        },
        field_name: None,
        ..Default::default()
    };
    let mut objects = vec![
        object("Text", 18.0, 20.0, 42.0, 240.0),
        object("Button", 18.0, 440.0, 42.0, 540.0),
        object("Rectangle", 72.0, 12.0, 236.0, 548.0),
    ];
    for (row, right) in [420.0, 420.0, 420.0, 300.0].into_iter().enumerate() {
        let top = 92.0 + row as f64 * 36.0;
        objects.push(object("Text", top, 24.0, top + 22.0, 124.0));
        let mut field = object("Field", top, 132.0, top + 22.0, right);
        field.field_name = Some(["First Name", "Last Name", "Email", "Created"][row].into());
        objects.push(field);
    }
    objects.push(object("Text", 258.0, 20.0, 278.0, 200.0));
    LayoutGeometry {
        revision: None,
        width: 560.0,
        height: 288.0,
        parts: vec![
            part("Header", 0.0, 60.0),
            part("Body", 60.0, 188.0),
            part("Footer", 248.0, 40.0),
        ],
        objects,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened() -> MockProvider {
        let mut mock = MockProvider::new();
        mock.open(&Connection {
            host: "fms.example.com".into(),
            port: 5003,
            file: "Contacts.fmp12".into(),
            account: "admin".into(),
            password: String::new(),
        })
        .unwrap();
        mock
    }

    #[test]
    fn creates_updates_clears_and_deletes_the_same_record_across_layouts() {
        let mut mock = opened();
        let created = mock
            .create_record(1, [("First Name".into(), Some("New".into()))].into())
            .unwrap();
        assert_eq!(created.id, 43);
        assert_eq!(mock.records(2, 0, 100).unwrap().total, 43);
        let changed = mock
            .update_record(
                2,
                created.id,
                [
                    ("First Name".into(), None),
                    ("Last Name".into(), Some("Edited".into())),
                ]
                .into(),
            )
            .unwrap();
        assert_eq!(changed.values[0], None);
        assert_eq!(changed.values[1].as_deref(), Some("Edited"));
        mock.delete_record(1, created.id).unwrap();
        let remaining = mock.records(2, 0, 100).unwrap();
        assert_eq!(remaining.total, 42);
        assert!(!remaining.records.iter().any(|r| r.id == created.id));
        assert!(mock.delete_record(1, created.id).is_err());
    }

    #[test]
    fn records_read_by_id_and_located_in_the_found_set() {
        let mut provider = opened();
        let record = provider.record_by_id(1, 7).unwrap();
        assert_eq!(record.total, 1);
        assert_eq!(record.records[0].values[0].as_deref(), Some("First6"));
        assert_eq!(record.fields, provider.records(1, 0, 1).unwrap().fields);
        assert_eq!(provider.record_position(1, 7).unwrap(), Some(6));
        provider
            .change_found_set(1, crate::found::FoundSetChange::Omit(vec![1, 2]))
            .unwrap();
        assert_eq!(provider.record_position(1, 7).unwrap(), Some(4));
        assert_eq!(provider.record_position(1, 2).unwrap(), None);
        provider.delete_record(1, 7).unwrap();
        assert!(provider.record_by_id(1, 7).unwrap().records.is_empty());
        assert_eq!(provider.record_position(1, 7).unwrap(), None);
    }

    #[test]
    fn invalid_changes_do_not_partially_modify_a_record() {
        let mut mock = opened();
        assert!(mock
            .update_record(
                1,
                1,
                [
                    ("First Name".into(), Some("Bad".into())),
                    ("unknown".into(), Some("field".into()))
                ]
                .into()
            )
            .is_err());
        assert_eq!(
            mock.records(1, 0, 1).unwrap().records[0].values[0].as_deref(),
            Some("First0")
        );
        assert!(mock
            .create_record(1, [("unknown".into(), None)].into())
            .is_err());
        assert_eq!(mock.records(1, 0, 100).unwrap().total, 42);
        mock.close();
        assert!(mock.delete_record(1, 1).is_err());
    }

    #[test]
    fn schema_changes_preserve_record_values_and_update_record_fields() {
        use crate::SchemaChange;
        let mut mock = opened();
        mock.apply_schema_change(SchemaChange::ChangeField {
            table_id: 0x8002,
            field_id: 1,
            name: "Given Name".into(),
            data_type: FieldKind::Text,
            comment: String::new(),
        })
        .unwrap();
        // Staged changes are not the file's until they are committed.
        assert_eq!(mock.records(1, 0, 1).unwrap().fields[0].name, "First Name");
        mock.commit_schema_changes().unwrap();
        let page = mock.records(1, 0, 1).unwrap();
        assert_eq!(page.fields[0].name, "Given Name");
        assert_eq!(page.records[0].values[0].as_deref(), Some("First0"));
        let schema = mock
            .apply_schema_change(SchemaChange::CreateField {
                table_id: 0x8002,
                name: "Score".into(),
                data_type: FieldKind::Number,
                comment: String::new(),
            })
            .unwrap();
        mock.commit_schema_changes().unwrap();
        let field_id = schema.table(0x8002).unwrap().fields.last().unwrap().id;
        let changed = mock
            .update_record(1, 1, [("Score".into(), Some("9".into()))].into())
            .unwrap();
        assert_eq!(changed.values.last().unwrap().as_deref(), Some("9"));
        mock.apply_schema_change(SchemaChange::DeleteField {
            table_id: 0x8002,
            field_id,
        })
        .unwrap();
        mock.commit_schema_changes().unwrap();
        assert_eq!(mock.records(1, 0, 1).unwrap().records[0].values.len(), 4);
        assert!(mock
            .update_record(1, 1, [("First Name".into(), None)].into())
            .is_err());
    }

    #[test]
    fn the_contact_card_calculates_from_the_sample_records() {
        use crate::calc::eval::{display, evaluate_text, Context, Variables};
        use crate::host::{CachedSource, FileCalc, FileHost, SessionInfo};
        use std::cell::RefCell;
        let mut mock = opened();
        let schema = mock.calc_schema().unwrap().unwrap();
        let card = mock.render_layout(CONTACT_CARD).unwrap().unwrap();
        let objects = &card.layout.objects;
        assert!(objects.get(7).unwrap().calcs.hide.is_some());
        let last = objects.get(6).unwrap();
        assert!(last.calcs.tooltip.is_some());
        assert!(last.calcs.conditions[0].formula.is_some());
        assert_eq!(mock.render_layout(1).unwrap(), None);

        let needs = [CalcNeed::Record {
            table: 0x8002,
            record: 2,
        }];
        let cache = RefCell::new(mock.calc_data(&needs).unwrap());
        let source = CachedSource::new(&cache);
        let session = SessionInfo::default();
        let (mut locals, mut globals) = (Variables::new(), Variables::new());
        let today = crate::host::session::today(0);
        let mut host = FileHost::new(
            FileCalc::new(&schema, &source, today),
            &session,
            &mut locals,
            &mut globals,
        );
        let result = evaluate_text("Full Name", &mut host, &Context::record(0xd0_0002, 2));
        assert_eq!(display(&result.value, &host), "First1 Last1");
        assert!(!source.has_missing());
    }

    #[test]
    fn only_contact_details_has_geometry() {
        let mut mock = opened();
        assert!(mock.layout_geometry(CONTACT_DETAILS).unwrap().is_some());
        assert_eq!(mock.layout_geometry(2).unwrap(), None);
        mock.close();
        assert!(mock.layout_geometry(CONTACT_DETAILS).is_err());
    }

    #[test]
    fn contact_details_parts_stack_and_hold_every_object() {
        let geometry = opened().layout_geometry(CONTACT_DETAILS).unwrap().unwrap();
        let mut bottom = 0.0;
        for part in &geometry.parts {
            assert_eq!(
                part.top, bottom,
                "{} follows the part above",
                part.kind_name
            );
            bottom = part.top + part.height;
        }
        assert_eq!(bottom, geometry.height);
        for object in &geometry.objects {
            let rect = object.rect;
            assert!(rect.width() > 0.0 && rect.height() > 0.0);
            assert!(rect.left >= 0.0 && rect.right <= geometry.width);
            assert!(rect.top >= 0.0 && rect.bottom <= geometry.height);
        }
    }
}
