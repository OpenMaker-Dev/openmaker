//! Calculations in the sample file: its calculation schema (the Contacts
//! table with an unstored "Full Name" calculation), data for calculations
//! from its records, and the styled "Contact Card" layout, whose objects use
//! a hide condition, conditional formatting, a tooltip and a symbol.

use std::collections::BTreeMap;

use om_calc::{Ctx, Locale, ValueType};
use om_formula::{Formula, Key};
use om_layout::style::ComponentStyle;
use om_layout::{
    CfCondition, Color, ConditionalFormat, FieldObject, FieldSpec, Layout, LayoutDef, LayoutObject,
    ObjectCalcs, ObjectStore, ObjectStyle, Part, Property, Rect, StyleSet,
};

use crate::host::{
    stored_value, CalcData, CalcNeed, CalcSchema, FieldDef, MemorySource, RecordValues, Recorder,
};
use crate::{DataError, Field, FieldKind, Record, RenderLayout};

/// The sample's base table and its occurrence.
pub(crate) const CONTACTS: u64 = 0x8002;
pub(crate) const CONTACTS_TO: u64 = 0xd0_0002;
/// The occurrence as a layout's `tableAliasKey` (layout-model.md 4.1).
const CONTACTS_ALIAS: u64 = 0x10_4082;
/// The unstored calculation field.
pub(crate) const FULL_NAME: u64 = 100;
/// The "Contact Card" layout.
pub(crate) const CONTACT_CARD: u32 = 3;

pub(crate) fn value_type(kind: FieldKind) -> ValueType {
    match kind {
        FieldKind::Text => ValueType::Text,
        FieldKind::Number => ValueType::Number,
        FieldKind::Date => ValueType::Date,
        FieldKind::Time => ValueType::Time,
        FieldKind::Timestamp => ValueType::Timestamp,
        FieldKind::Container => ValueType::Container,
    }
}

fn compile(schema: &CalcSchema, occurrence: Option<u64>, source: &str) -> Option<Formula> {
    let mut ctx = om_formula::Context::new(schema.names());
    ctx.occurrence = occurrence;
    ctx.support_self = true;
    let expr = om_formula::compile(source, &ctx).ok()?.expr;
    Some(Formula {
        context: Some(Key::from_id(occurrence.unwrap_or(0))),
        formatted: None,
        polish: expr,
    })
}

/// The sample's calculation schema over its current fields (the record
/// fields, in order, with ids from 1). "Full Name" joins fields 1 and 2.
pub(crate) fn schema(fields: &[Field], ids: &[u64]) -> CalcSchema {
    let mut schema = CalcSchema::new("Contacts", Locale::en_us());
    schema.add_table(CONTACTS, "Contacts");
    schema.add_occurrence(CONTACTS_TO, "Contacts", CONTACTS);
    for (field, id) in fields.iter().zip(ids) {
        schema.add_field(
            CONTACTS,
            FieldDef::data(*id, field.name.clone(), value_type(field.kind)),
        );
    }
    let name = |id: u64| {
        fields
            .iter()
            .zip(ids)
            .find(|(_, i)| **i == id)
            .map(|(f, _)| f.name.clone())
    };
    if let (Some(first), Some(last)) = (name(1), name(2)) {
        let source = format!("${{{first}}} & \" \" & ${{{last}}}");
        if let Some(formula) = compile(&schema, Some(CONTACTS_TO), &source) {
            schema.add_field(
                CONTACTS,
                FieldDef::calculation(FULL_NAME, "Full Name", ValueType::Text, formula, false),
            );
        }
    }
    schema.add_layout(1, "Contact Details", Some(CONTACTS_TO));
    schema.add_layout(2, "Contacts List", Some(CONTACTS_TO));
    schema.add_layout(CONTACT_CARD, "Contact Card", Some(CONTACTS_TO));
    schema
}

/// Data for `needs` from the sample's records.
pub(crate) fn data(
    schema: &CalcSchema,
    fields: &[Field],
    ids: &[u64],
    records: &[Record],
    needs: &[CalcNeed],
) -> Result<CalcData, DataError> {
    let today = crate::host::session::today(0);
    let cx = Ctx::new(&schema.locale, today);
    let mut source = MemorySource::new();
    for record in records {
        let mut values = RecordValues::default();
        for ((field, id), value) in fields.iter().zip(ids).zip(&record.values) {
            if let Some(text) = value {
                values.set(*id, 1, stored_value(text, value_type(field.kind), &cx));
            }
        }
        source.put_record(CONTACTS, record.id as u64, values);
    }
    let recorder = Recorder::new(&source);
    for need in needs {
        recorder
            .fetch(schema, need, &cx)
            .map_err(|e| DataError::new(format!("The sample has no data for {need:?} ({e}).")))?;
    }
    Ok(recorder.take())
}

fn object(id: u64, object_type: u32, top: f64, left: f64, width: f64) -> LayoutObject {
    LayoutObject {
        id,
        object_type,
        bounds: Some(Rect {
            top,
            left,
            bottom: top + 26.0,
            right: left + width,
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn field(id: u64, field: u64, top: f64) -> LayoutObject {
    LayoutObject {
        field_obj: Some(FieldObject {
            field_spec: Some(FieldSpec {
                table_key: CONTACTS_ALIAS,
                field_key: field,
                repetition: 1,
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..object(id, 1, top, 150.0, 260.0)
    }
}

fn text(id: u64, text: &str, top: f64) -> LayoutObject {
    LayoutObject {
        text: Some(text.into()),
        ..object(id, 2, top, 20.0, 120.0)
    }
}

/// The background conditional formatting paints: red.
pub const CONDITION_FILL: Color = Color {
    red: 0.8,
    green: 0.1,
    blue: 0.1,
    alpha: 1.0,
};

/// "Contact Card": labels and fields for First Name, Full Name (the
/// unstored calculation) and Last Name; "Preferred contact" hidden unless
/// First Name is `First0`; Last Name red when it is `Last1`, with a
/// tooltip naming the contact; the record number as a `{{symbol}}`.
pub(crate) fn contact_card(schema: &CalcSchema) -> RenderLayout {
    let calc = |source: &str| compile(schema, None, source);
    let mut objects = vec![
        text(1, "First Name", 20.0),
        field(2, 1, 20.0),
        text(3, "Full Name", 60.0),
        field(4, FULL_NAME, 60.0),
        text(5, "Last Name", 100.0),
        field(6, 2, 100.0),
        text(7, "Preferred contact", 140.0),
        text(8, "Card {{RecordNumber}}", 180.0),
    ];
    objects[6].calcs = ObjectCalcs {
        hide: calc("Contacts::First Name <> \"First0\""),
        ..ObjectCalcs::default()
    };
    objects[5].flags = om_layout::calcs::TOOLTIP_FLAG;
    objects[5].calcs = ObjectCalcs {
        tooltip: calc("\"Last name of \" & Contacts::Full Name"),
        conditions: vec![ConditionalFormat {
            formula: calc("Self = \"Last1\""),
            condition: CfCondition {
                style: Some(ObjectStyle {
                    local_styles: Some(StyleSet {
                        components: vec![ComponentStyle {
                            component: 1,
                            state: 1,
                            properties: vec![Property {
                                id: 7,
                                color: Some(CONDITION_FILL),
                                ..Default::default()
                            }],
                            ..Default::default()
                        }],
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            kind: None,
        }],
        ..ObjectCalcs::default()
    };
    let root = objects.iter().map(|o| o.id).collect();
    RenderLayout {
        layout: Layout {
            id: u64::from(CONTACT_CARD),
            name: "Contact Card".into(),
            def: Some(LayoutDef {
                table_alias_key: CONTACTS_ALIAS,
                base_width: 560.0,
                ..Default::default()
            }),
            parts: vec![Part {
                id: 1,
                part_type: 4,
                top: 0.0,
                height: 230.0,
                ..Default::default()
            }],
            objects: ObjectStore {
                root,
                objects,
                ..Default::default()
            },
            ..Default::default()
        },
        field_bindings: BTreeMap::from([
            (2, "First Name".into()),
            (4, "Full Name".into()),
            (6, "Last Name".into()),
        ]),
    }
}
