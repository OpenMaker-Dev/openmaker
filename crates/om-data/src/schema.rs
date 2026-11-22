//! Database catalog exposed to Manage Database. Stable numeric identities are
//! retained when names change; unsupported native capabilities are explicit.

use crate::calc_field::CalculationField;
use crate::field_options::{AutoEnterValue, FieldOptions};
use crate::scripts::specs::SortSpec;
use crate::{DataError, FieldKind};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DatabaseSchema {
    pub tables: Vec<SchemaTable>,
    pub occurrences: Vec<SchemaOccurrence>,
    pub relationships: Vec<SchemaRelationship>,
    /// False distinguishes unread relationship predicates from an empty graph.
    pub relationships_decoded: bool,
    pub capabilities: SchemaCapabilities,
}

impl DatabaseSchema {
    /// Plan an equality match dragged between existing graph occurrences.
    /// Persistence uses the same Create/ChangeRelationship schema operations
    /// as Edit Relationship; this does not save a draft.
    pub fn link_fields(
        &self,
        from: (u64, u64),
        to: (u64, u64),
    ) -> Result<Option<SchemaChange>, DataError> {
        // The graph's own edits (Edit Relationship) need the occurrences
        // the host lets this account change, and predicates it could read.
        if !self.capabilities.occurrences || !self.relationships_decoded {
            return Err(DataError::with_code(
                3,
                "Relationship definitions cannot be changed here.",
            ));
        }
        if from.0 == to.0 {
            return Err(DataError::with_code(
                3,
                "Duplicate the table occurrence before relating its fields to itself.",
            ));
        }
        for (occurrence, field) in [from, to] {
            let definition = self
                .occurrences
                .iter()
                .find(|o| o.id == occurrence)
                .and_then(|o| o.table_id)
                .and_then(|t| self.table(t))
                .and_then(|t| t.fields.iter().find(|f| f.id == field));
            if !definition.is_some_and(|f| {
                f.data_type != FieldKind::Container
                    && f.kind != SchemaFieldKind::Summary
                    && !f.unstored
            }) {
                return Err(DataError::with_code(
                    3,
                    "The selected graph match field is unavailable or unsupported.",
                ));
            }
        }
        let existing = self.relationships.iter().find(|r| {
            (r.left_occurrence, r.right_occurrence) == (from.0, to.0)
                || (r.left_occurrence, r.right_occurrence) == (to.0, from.0)
        });
        let reversed = existing.is_some_and(|r| r.left_occurrence != from.0);
        let (left_field, right_field) = if reversed {
            (to.1, from.1)
        } else {
            (from.1, to.1)
        };
        let pair = RelationshipMatch {
            left_field,
            right_field,
            operator: 0,
        };
        Ok(Some(if let Some(r) = existing {
            let mut matches: Vec<_> = r
                .matches
                .iter()
                .map(|m| RelationshipMatch {
                    left_field: m.left_field,
                    right_field: m.right_field,
                    operator: m.operator,
                })
                .collect();
            if matches.contains(&pair) {
                return Ok(None);
            }
            matches.push(pair);
            let mut left = r.left();
            let mut right = r.right();
            // No sort edit was requested; retain even an unread stored order.
            left.order = None;
            right.order = None;
            SchemaChange::ChangeRelationship {
                relationship_id: r.id,
                matches,
                left,
                right,
            }
        } else {
            SchemaChange::CreateRelationship {
                left_occurrence: from.0,
                right_occurrence: to.0,
                matches: vec![pair],
                left: RelationshipSide::default(),
                right: RelationshipSide::default(),
            }
        }))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SchemaCapabilities {
    pub tables: bool,
    pub fields: bool,
    pub occurrences: bool,
    pub relationships: bool,
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaTable {
    pub id: u64,
    pub name: String,
    /// The table's comment (`[3, 16, 5, T, 03]`), empty for none.
    pub comment: String,
    pub fields: Vec<SchemaField>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaField {
    pub id: u64,
    pub name: String,
    pub kind: SchemaFieldKind,
    pub data_type: FieldKind,
    /// The field's comment (`[T, 3, 5, F, 03]`), empty for none.
    pub comment: String,
    /// The stored DDL annotation (`[T, 3, 5, F, 30]`), empty for none.
    pub annotation: String,
    /// Whether Customize field display names is on: bit `0x2` of the
    /// advanced flags `[T, 3, 5, F, 29]` (file-metadata.md 1.2).
    pub display_names_enabled: bool,
    /// Unique-value validation or an auto-entered serial number
    /// (schema-path.md 5.5): the relationships graph draws its end of an
    /// `=` relationship as the "one" side.
    pub unique: bool,
    /// A calculation whose result is not stored: it cannot be a match field.
    pub unstored: bool,
    /// A normal field's auto-enter, validation and storage options; `None`
    /// for calculation and summary fields and unreadable definitions.
    pub options: Option<FieldOptions>,
    /// A calculation field's formula and options; `None` for other fields
    /// and calculations whose definition or formula does not read.
    pub calculation: Option<CalculationField>,
    /// A summary field's definition `09`; `None` for other fields.
    pub summary: Option<crate::meta::SummaryDef>,
    /// The stored 26-byte definition `[T, 3, 5, F, 02]` as read, whose
    /// bits the Fields tab lists (client-ui.md); `None` where none was
    /// read, as for the sample provider's fields.
    pub definition: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SchemaFieldKind {
    #[default]
    Normal,
    Calculation,
    Summary,
    Unknown(u8),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaOccurrence {
    pub id: u64,
    pub name: String,
    pub table_id: Option<u64>,
    /// Where the relationships graph draws it; `None` when the definition
    /// holds no box OpenMaker can read.
    pub graph: Option<OccurrenceBox>,
}

/// An occurrence's box in the relationships graph, in points from the
/// graph's top left corner, as the file stores it (schema-path.md 5.3).
#[derive(Clone, Copy, Debug)]
pub struct OccurrenceBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    /// The height with every field shown.
    pub height: f32,
    /// The stored field-list height (`height` less the title bar and the
    /// key-field area in every sample).
    pub list_height: u32,
    /// The stored display word; see [`Self::shows`].
    pub display: u32,
    /// RGBA.
    pub color: [u8; 4],
}

/// What an occurrence's box lists under its title bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OccurrenceShows {
    /// The key fields, then every field.
    AllFields,
    /// The key fields only.
    KeyFields,
    /// Nothing but the title bar.
    NameOnly,
}

impl OccurrenceBox {
    /// FileMaker's default colour for a table occurrence, `78 78 78 ff`
    /// in every shipped template.
    pub const DEFAULT_COLOR: [u8; 4] = [0x78, 0x78, 0x78, 0xff];

    /// A new box at `(x, y)`, 131 x 118 points with every field shown,
    /// the size of most occurrences after the first in the shipped
    /// templates: OpenMaker's choice for occurrences it adds or places.
    pub fn new_at(x: f32, y: f32) -> Self {
        Self {
            x,
            y,
            width: 131.0,
            height: 118.0,
            list_height: 74,
            display: 0,
            color: Self::DEFAULT_COLOR,
        }
    }

    /// The display word read as FileMaker Pro 26 draws it: 0 every field
    /// and 1 the key fields only, as observed; 2 the name only, inferred
    /// from FileMaker's three display states and not observed; any other
    /// value every field.
    pub fn shows(&self) -> OccurrenceShows {
        match self.display {
            1 => OccurrenceShows::KeyFields,
            2 => OccurrenceShows::NameOnly,
            _ => OccurrenceShows::AllFields,
        }
    }
}

/// Coordinates compare by their bits, so that equality is total.
impl PartialEq for OccurrenceBox {
    fn eq(&self, other: &Self) -> bool {
        [self.x, self.y, self.width, self.height].map(f32::to_bits)
            == [other.x, other.y, other.width, other.height].map(f32::to_bits)
            && (self.list_height, self.display, self.color)
                == (other.list_height, other.display, other.color)
    }
}

impl Eq for OccurrenceBox {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaRelationship {
    pub id: u64,
    pub left_occurrence: u64,
    pub right_occurrence: u64,
    pub matches: Vec<SchemaMatch>,
    /// The definition's option word (file-metadata.md 4.1); read it with
    /// [`SchemaRelationship::left`] and [`SchemaRelationship::right`].
    pub options: u32,
    /// The stored sort orders of the left and of the right occurrence's
    /// records (`mRightToLeftOrder`, `mLeftToRightOrder`); `None` where one
    /// does not read.
    pub orders: [Option<SortSpec>; 2],
}

impl SchemaRelationship {
    /// Edit Relationship's options for the left occurrence's records.
    pub fn left(&self) -> RelationshipSide {
        RelationshipSide {
            order: self.orders[0].clone(),
            ..RelationshipSide::from_options(self.options, false)
        }
    }

    /// Edit Relationship's options for the right occurrence's records.
    pub fn right(&self) -> RelationshipSide {
        RelationshipSide {
            order: self.orders[1].clone(),
            ..RelationshipSide::from_options(self.options, true)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaMatch {
    pub left_field: u64,
    pub right_field: u64,
    pub comparison: String,
    /// The stored operator code, 0 `=` to 6 `×` (`meta::JoinOp`).
    pub operator: u8,
}

/// A match pair Edit Relationship sends: `left_field operator right_field`
/// with the operator's code (file-metadata.md 4.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RelationshipMatch {
    pub left_field: u64,
    pub operator: u8,
    pub right_field: u64,
}

/// One side's checkboxes in Edit Relationship: what happens to this side's
/// records when working from the other side (file-metadata.md 4.1).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RelationshipSide {
    /// "Allow creation of records in this table via this relationship".
    pub allow_creation: bool,
    /// "Delete related records in this table when a record is deleted in
    /// the other table".
    pub delete_related: bool,
    /// "Sort records", by the order the definition keeps for this side.
    pub sort: bool,
    /// The order "Specify…" chose for this side's records, of this side's
    /// occurrence's fields; `None` keeps the stored order (none for a new
    /// relationship). The definition keeps an order when "Sort records" is
    /// off, as FileMaker does.
    pub order: Option<SortSpec>,
}

impl RelationshipSide {
    /// The right side's options are the left-to-right bits (`0x01`
    /// delete, `0x10` create, `0x40` sort), the left side's the
    /// right-to-left ones (`0x02`, `0x20`, `0x80`).
    pub fn from_options(options: u32, right: bool) -> Self {
        let shift = if right { 0 } else { 1 };
        Self {
            delete_related: options & (0x01 << shift) != 0,
            allow_creation: options & (0x10 << shift) != 0,
            sort: options & (0x40 << shift) != 0,
            order: None,
        }
    }

    /// This side's bits of the option word.
    pub fn bits(&self, right: bool) -> u32 {
        let shift = if right { 0 } else { 1 };
        let bit = |on: bool, bit: u32| if on { bit } else { 0 };
        (bit(self.delete_related, 0x01) | bit(self.allow_creation, 0x10) | bit(self.sort, 0x40))
            << shift
    }
}

/// One edit Manage Database makes to the schema: Create, Change and Delete
/// on the Tables, Fields and Relationships tabs. Comments are empty for
/// none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaChange {
    CreateTable {
        name: String,
        comment: String,
    },
    /// Change a table's name and comment. Occurrences named as the table
    /// was are renamed with it, as FileMaker Pro renames them.
    ChangeTable {
        table_id: u64,
        name: String,
        comment: String,
    },
    /// Delete a table with its fields and records. Its occurrences stay in
    /// the graph, showing a missing table, unless `remove_occurrences`
    /// (FileMaker Pro's "Also remove occurrences of these tables in the
    /// graph").
    DeleteTable {
        table_id: u64,
        remove_occurrences: bool,
    },
    CreateField {
        table_id: u64,
        name: String,
        data_type: FieldKind,
        comment: String,
    },
    ChangeField {
        table_id: u64,
        field_id: u64,
        name: String,
        data_type: FieldKind,
        comment: String,
    },
    DeleteField {
        table_id: u64,
        field_id: u64,
    },
    /// Duplicate in the Fields tab: a field of the same kind and type named
    /// [`duplicate_name`], with the original's comment and its options,
    /// formula or summary definition; none of its values.
    DuplicateField {
        table_id: u64,
        field_id: u64,
    },
    /// Specify Calculation's OK for a new calculation field (Create with
    /// the type Calculation). The formula is compiled against the schema
    /// as edited so far.
    CreateCalculationField {
        table_id: u64,
        name: String,
        comment: String,
        calculation: CalculationField,
    },
    /// Specify Calculation's OK for an existing calculation field: its
    /// formula, context, result and options. Its name and comment change
    /// with [`SchemaChange::ChangeField`].
    ChangeCalculation {
        table_id: u64,
        field_id: u64,
        calculation: CalculationField,
    },
    /// Options for Summary Field's OK for a new summary field (Create with
    /// the type Summary): what it summarizes and how.
    CreateSummaryField {
        table_id: u64,
        name: String,
        comment: String,
        summary: crate::meta::SummaryDef,
    },
    /// Options for Summary Field's OK for an existing summary field.
    ChangeSummary {
        table_id: u64,
        field_id: u64,
        summary: crate::meta::SummaryDef,
    },
    /// Options for Field: a normal field's auto-enter, validation and
    /// storage options (the OK of FileMaker Pro's "Options for Field").
    SetFieldOptions {
        table_id: u64,
        field_id: u64,
        /// Boxed: the options are much larger than the other changes.
        options: Box<FieldOptions>,
    },
    CreateOccurrence {
        table_id: u64,
        name: String,
    },
    RenameOccurrence {
        occurrence_id: u64,
        name: String,
    },
    DeleteOccurrence {
        occurrence_id: u64,
    },
    /// Relate two occurrences (Edit Relationship's OK for a new one).
    /// FileMaker allows one path between two occurrences, so occurrences
    /// already connected are refused.
    CreateRelationship {
        left_occurrence: u64,
        right_occurrence: u64,
        matches: Vec<RelationshipMatch>,
        left: RelationshipSide,
        right: RelationshipSide,
    },
    /// Change a relationship's match pairs and options; its occurrences
    /// stay.
    ChangeRelationship {
        relationship_id: u64,
        matches: Vec<RelationshipMatch>,
        left: RelationshipSide,
        right: RelationshipSide,
    },
    DeleteRelationship {
        relationship_id: u64,
    },
    /// Move or resize an occurrence's box in the graph (whole points).
    ArrangeOccurrence {
        occurrence_id: u64,
        left: i32,
        top: i32,
        width: i32,
        height: i32,
    },
    /// Colour an occurrence's box, as RGBA.
    ColorOccurrence {
        occurrence_id: u64,
        color: [u8; 4],
    },
    /// The graph's Duplicate: a copy of each occurrence (its table and data
    /// source, colour and box, moved by [`DUPLICATE_OFFSET`]), named as a
    /// new occurrence of its table is ([`unused_name`]), and a copy of each
    /// relationship joining two of them, with its match pairs, options and
    /// sort orders. All or nothing.
    DuplicateOccurrences {
        occurrence_ids: Vec<u64>,
    },
    /// Move or resize several occurrences' boxes at once (the graph's
    /// Arrange tools, [`arrange`]): each `(occurrence, [left, top, width,
    /// height])` in whole points. All or nothing.
    ArrangeOccurrences {
        frames: Vec<(u64, [i32; 4])>,
    },
}

/// One of the relationships graph's Arrange tools (Claris Help, "Working with
/// the relationships graph": align left edges, centers or right edges; top
/// edges, centers or bottom edges; distribute horizontally or vertically;
/// resize to the smallest or largest width, height, or both).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrangement {
    AlignLeft,
    AlignCenter,
    AlignRight,
    AlignTop,
    AlignMiddle,
    AlignBottom,
    DistributeHorizontally,
    DistributeVertically,
    Resize {
        largest: bool,
        width: bool,
        height: bool,
    },
}

/// The graph's Arrange: boxes `frames` (occurrence, [left, top, width,
/// height] as the graph draws them) arranged by `how`, as one change of
/// their boxes in whole points. Edges align to the selection's outermost
/// edge and centers to its bounds' center; distributing keeps the two
/// outermost boxes and spaces the others evenly between them (FileMaker's
/// layout tools, inferred for the graph). Resize keeps each box's top left.
pub fn arrange(frames: &[(u64, [f32; 4])], how: Arrangement) -> Result<SchemaChange, DataError> {
    if frames.len() < 2 {
        return Err(DataError::with_code(
            3,
            "Select two or more table occurrences to arrange.",
        ));
    }
    // A box with a size, at a place a whole point can hold: before and after.
    let usable = |f: &[f32; 4]| {
        f.iter().all(|v| v.is_finite() && v.abs() <= 1.0e9) && f[2] > 0.0 && f[3] > 0.0
    };
    let unusable =
        || DataError::with_code(3, "A selected table occurrence's box cannot be arranged.");
    if !frames.iter().all(|(_, f)| usable(f)) {
        return Err(unusable());
    }
    let left = frames
        .iter()
        .map(|(_, f)| f[0])
        .fold(f32::INFINITY, f32::min);
    let top = frames
        .iter()
        .map(|(_, f)| f[1])
        .fold(f32::INFINITY, f32::min);
    let right = frames
        .iter()
        .map(|(_, f)| f[0] + f[2])
        .fold(f32::NEG_INFINITY, f32::max);
    let bottom = frames
        .iter()
        .map(|(_, f)| f[1] + f[3])
        .fold(f32::NEG_INFINITY, f32::max);
    let mut out: Vec<(u64, [f32; 4])> = frames.to_vec();
    let distribute = |out: &mut Vec<(u64, [f32; 4])>, axis: usize| {
        out.sort_by(|a, b| a.1[axis].total_cmp(&b.1[axis]));
        let n = out.len();
        let span = out[n - 1].1[axis] + out[n - 1].1[axis + 2] - out[0].1[axis];
        let sizes: f32 = out.iter().map(|(_, f)| f[axis + 2]).sum();
        let gap = (span - sizes) / (n - 1) as f32;
        let mut at = out[0].1[axis];
        for (_, frame) in out.iter_mut() {
            frame[axis] = at;
            at += frame[axis + 2] + gap;
        }
    };
    match how {
        Arrangement::AlignLeft => out.iter_mut().for_each(|(_, f)| f[0] = left),
        Arrangement::AlignRight => out.iter_mut().for_each(|(_, f)| f[0] = right - f[2]),
        Arrangement::AlignCenter => out
            .iter_mut()
            .for_each(|(_, f)| f[0] = (left + right) / 2.0 - f[2] / 2.0),
        Arrangement::AlignTop => out.iter_mut().for_each(|(_, f)| f[1] = top),
        Arrangement::AlignBottom => out.iter_mut().for_each(|(_, f)| f[1] = bottom - f[3]),
        Arrangement::AlignMiddle => out
            .iter_mut()
            .for_each(|(_, f)| f[1] = (top + bottom) / 2.0 - f[3] / 2.0),
        Arrangement::DistributeHorizontally => distribute(&mut out, 0),
        Arrangement::DistributeVertically => distribute(&mut out, 1),
        Arrangement::Resize {
            largest,
            width,
            height,
        } => {
            let pick = |side: usize| {
                let sizes = frames.iter().map(|(_, f)| f[side]);
                match largest {
                    true => sizes.fold(f32::NEG_INFINITY, f32::max),
                    false => sizes.fold(f32::INFINITY, f32::min),
                }
            };
            let (w, h) = (pick(2), pick(3));
            for (_, f) in out.iter_mut() {
                if width {
                    f[2] = w;
                }
                if height {
                    f[3] = h;
                }
            }
        }
    }
    if !out.iter().all(|(_, f)| usable(f)) {
        return Err(unusable());
    }
    Ok(SchemaChange::ArrangeOccurrences {
        frames: out
            .into_iter()
            .map(|(id, f)| (id, f.map(|v| v.round() as i32)))
            .collect(),
    })
}

/// How far the graph's Duplicate moves the copies, right and down, in
/// points: below and to the right of the originals, their relative
/// positions kept (Claris Help). The distance is OpenMaker's choice.
pub const DUPLICATE_OFFSET: i32 = 20;

/// What the third column of Manage Database's Fields tab shows; a click on
/// its header steps to the next (client-ui.md, "Manage Database: the Fields
/// tab's third column").
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FieldColumn {
    #[default]
    Options,
    Comments,
    Advanced,
}

impl FieldColumn {
    /// The column's title, whatever it shows (three spaces before the
    /// parenthesis, as FileMaker Pro's resource has them).
    pub const TITLE: &'static str = "Options / Comments / Advanced   (Click to toggle)";

    pub fn next(self) -> FieldColumn {
        match self {
            FieldColumn::Options => FieldColumn::Comments,
            FieldColumn::Comments => FieldColumn::Advanced,
            FieldColumn::Advanced => FieldColumn::Options,
        }
    }
}

impl DatabaseSchema {
    /// The text the Fields tab's third column shows for `field` of `table`
    /// in mode `column` (client-ui.md, "Manage Database: the Fields tab's
    /// third column").
    pub fn field_column_text(
        &self,
        table: &SchemaTable,
        field: &SchemaField,
        column: FieldColumn,
    ) -> String {
        match column {
            FieldColumn::Options => self.field_options_text(table, field),
            FieldColumn::Comments => field.comment.clone(),
            // Which member FileMaker shows after "Display Names", and alone
            // without the option, is not traced (client-ui.md): that it is
            // the annotation, and the ", " between them, are inferred
            // ([#611](https://github.com/OpenMaker-Dev/openmaker/issues/611)).
            FieldColumn::Advanced if field.display_names_enabled => {
                if field.annotation.is_empty() {
                    "Display Names".into()
                } else {
                    format!("Display Names, {}", field.annotation)
                }
            }
            FieldColumn::Advanced => field.annotation.clone(),
        }
    }

    /// FileMaker Pro's options text for `field` of `table`: its options as
    /// a list joined by ", " (`FMFieldListData::GetOptionsText`).
    pub fn field_options_text(&self, table: &SchemaTable, field: &SchemaField) -> String {
        use crate::field_options::EntryValue;
        // FileMaker tests the stored bits, which say more than the
        // dialog's model (two auto-enter options at once, relaxed indexes);
        // a field read without them is described by its decoded options.
        let definition = field.definition.clone().or_else(|| {
            let mut definition =
                crate::field_catalog::definition(crate::field_catalog::ScalarType::Text, 21);
            definition[1] = crate::calc_field::type_code(field.data_type);
            match (&field.options, &field.calculation) {
                (_, Some(calculation)) if field.kind == SchemaFieldKind::Calculation => {
                    calculation.encode(&mut definition)
                }
                (Some(options), _) => options.encode(&mut definition),
                _ => return None,
            }
            Some(definition)
        });
        let word = |at: usize| {
            definition
                .as_deref()
                .and_then(|d| d.get(at..at + 4))
                .map_or(0, |w| u32::from_be_bytes(w.try_into().expect("4 bytes")))
        };
        let (options, rules) = (word(8), word(12));
        let entry = definition.as_deref().and_then(|d| d.get(3)).copied();
        let mut items: Vec<String> = Vec::new();
        let mut add = |item: &str| items.push(item.to_owned());
        for (bits, item) in [
            (0x1_0000, "Global"),
            (0x2_0000, "Unstored"),
            (0xb800_0000, "Indexed"),
        ] {
            if options & bits != 0 {
                add(item);
            }
        }
        let name_of = |id: u64| {
            table
                .fields
                .iter()
                .find(|f| f.id == id)
                .map_or_else(String::new, |f| f.name.clone())
        };
        match field.kind {
            SchemaFieldKind::Normal => {
                if options & 0x1000 != 0 {
                    add(if options & 0x2000 != 0 {
                        "External (Open)"
                    } else {
                        "External (Secure)"
                    });
                }
                if options & 0x40 != 0 {
                    add("Use Furigana");
                }
                if options & 0x1 != 0 {
                    // Byte 3: 0-4 on creation, 5-9 on modification.
                    let entry = usize::from(entry.unwrap_or(0));
                    if let Some(value) = EntryValue::ALL.get(entry % 5).filter(|_| entry < 10) {
                        let when = if entry < 5 {
                            "Creation"
                        } else {
                            "Modification"
                        };
                        add(&format!("{when} {}", value.label()));
                    }
                }
                if options & 0x202 != 0 {
                    add("Auto-enter Serial");
                }
                if options & 0x4 != 0 {
                    add("Auto-enter Data");
                }
                if options & 0x8 != 0 {
                    add(if options & 0x80 != 0 {
                        "Auto-enter Calculation replaces existing value"
                    } else {
                        "Auto-enter Calculation"
                    });
                }
                for (bits, item) in [
                    (0x10, "Auto-enter Last Visited"),
                    (0x20, "Evaluate Always"),
                    (0x100, "Can't Modify Auto"),
                    (0x400, "Lookup"),
                ] {
                    if options & bits != 0 {
                        add(item);
                    }
                }
                for (bits, item) in [
                    (0x400, "Always Validate"),
                    (0x1, "By Calculation"),
                    (0x100, "By Value List"),
                    (0x8, "Required Value"),
                    (0x10, "Unique"),
                    (0x20, "Existing"),
                ] {
                    if rules & bits != 0 {
                        add(item);
                    }
                }
                if rules & 0x7000 != 0 {
                    add(if rules & 0x1000 != 0 {
                        "Numeric Only"
                    } else if rules & 0x2000 != 0 {
                        "4-Digit Year Date"
                    } else {
                        "Time of Day"
                    });
                }
                for (bits, item) in [(0x40, "Range"), (0x200, "Maximum")] {
                    if rules & bits != 0 {
                        add(item);
                    }
                }
                if rules & 0x4 == 0 && rules & 0x7379 != 0 {
                    add("Allow Override");
                }
                if rules & 0x8000 != 0 {
                    add("Message by Calculation");
                } else if rules & 0x80 != 0 {
                    add("Message");
                }
            }
            SchemaFieldKind::Calculation => {
                let Some(calculation) = &field.calculation else {
                    return items.join(", ");
                };
                // The context is named when the table has more than one
                // occurrence (`RebuildList` counts them).
                match self
                    .occurrences
                    .iter()
                    .find(|o| o.id == calculation.context)
                {
                    None => add("from <unknown>"),
                    Some(context) => {
                        let occurrences = self
                            .occurrences
                            .iter()
                            .filter(|o| o.table_id == Some(table.id))
                            .count();
                        if occurrences >= 2 {
                            add(&format!("from {}", context.name));
                        }
                    }
                }
                // `StripNonPrintingChars`: a return reads as a space, a line
                // feed is dropped; tabs stay.
                let formula = calculation.formula.replace('\n', "").replace('\r', " ");
                add(&format!("= {formula}"));
            }
            SchemaFieldKind::Summary => {
                use crate::meta::{summary_flag, SummaryKind};
                let Some(summary) = &field.summary else {
                    return items.join(", ");
                };
                let field_name = |f: &Option<crate::meta::SummaryField>| {
                    f.map_or_else(String::new, |f| name_of(f.field))
                };
                let kind = match summary.kind {
                    SummaryKind::Total => "Total of ",
                    SummaryKind::Average => "Average of ",
                    SummaryKind::Count => "Count of ",
                    SummaryKind::Minimum => "Minimum of ",
                    SummaryKind::Maximum => "Maximum of ",
                    SummaryKind::StdDeviation => "Standard Deviation of ",
                    SummaryKind::Fraction => "Fraction of Total of ",
                    SummaryKind::List => "List of ",
                    SummaryKind::Other(_) => "",
                };
                // "= " is not an item: no separator before it.
                let mut text = items.join(", ");
                text.push_str("= ");
                text.push_str(kind);
                text.push_str(&field_name(&summary.field));
                let option = summary.has(summary_flag::OPTION);
                let running = summary.has(summary_flag::RUNNING);
                let restart = summary.has(summary_flag::RESTART);
                if option || (running && summary.kind != SummaryKind::List) {
                    let suffix = match summary.kind {
                        SummaryKind::StdDeviation => " (by population)",
                        SummaryKind::List => "",
                        _ if restart => " (running with restart), when sorted by ",
                        SummaryKind::Total | SummaryKind::Count => " (running)",
                        SummaryKind::Average => ", weighted by ",
                        SummaryKind::Fraction => ", when sorted by ",
                        _ => "",
                    };
                    text.push_str(suffix);
                    let second = match summary.kind {
                        SummaryKind::Average | SummaryKind::Fraction => true,
                        SummaryKind::Total | SummaryKind::Count => restart,
                        _ => false,
                    };
                    if second && summary.kind != SummaryKind::StdDeviation {
                        text.push_str(&field_name(&summary.additional));
                    }
                }
                return text;
            }
            SchemaFieldKind::Unknown(_) => {}
        }
        items.join(", ")
    }

    pub fn table(&self, id: u64) -> Option<&SchemaTable> {
        self.tables.iter().find(|table| table.id == id)
    }

    /// The sample provider's transactional catalog editor. Native providers
    /// must perform their server transaction and re-read, not call this helper.
    pub fn apply_sample_change(&mut self, change: SchemaChange) -> Result<(), DataError> {
        let mut next = self.clone();
        next.apply_sample_change_inner(change)?;
        *self = next;
        Ok(())
    }

    fn apply_sample_change_inner(&mut self, change: SchemaChange) -> Result<(), DataError> {
        match change {
            SchemaChange::CreateTable { name, comment } => {
                unique_name(&name, self.tables.iter().map(|t| t.name.as_str()))?;
                let id = self
                    .tables
                    .iter()
                    .map(|t| t.id)
                    .max()
                    .unwrap_or(0x8001)
                    .checked_add(1)
                    .ok_or_else(|| DataError::new("No table IDs are available."))?;
                let name = name.trim().to_owned();
                self.tables.push(SchemaTable {
                    id,
                    name: name.clone(),
                    comment,
                    fields: vec![],
                });
                // FileMaker Pro adds an occurrence of a new table to the graph.
                let occurrence = unused_name(&name, self.occurrences.iter().map(|o| &o.name));
                self.apply_sample_change_inner(SchemaChange::CreateOccurrence {
                    table_id: id,
                    name: occurrence,
                })?;
            }
            SchemaChange::ChangeTable {
                table_id,
                name,
                comment,
            } => {
                unique_name(
                    &name,
                    self.tables
                        .iter()
                        .filter(|t| t.id != table_id)
                        .map(|t| t.name.as_str()),
                )?;
                let name = name.trim().to_owned();
                let table = self.table_mut(table_id)?;
                let old = std::mem::replace(&mut table.name, name.clone());
                table.comment = comment;
                if old != name {
                    let taken = |o: &SchemaOccurrence| o.name.to_lowercase() == name.to_lowercase();
                    if !self.occurrences.iter().any(taken) {
                        for occurrence in &mut self.occurrences {
                            if occurrence.table_id == Some(table_id) && occurrence.name == old {
                                occurrence.name.clone_from(&name);
                            }
                        }
                    }
                }
            }
            SchemaChange::DeleteTable {
                table_id,
                remove_occurrences,
            } => {
                self.table_mut(table_id)?;
                self.tables.retain(|t| t.id != table_id);
                let mine: Vec<u64> = self
                    .occurrences
                    .iter()
                    .filter(|o| o.table_id == Some(table_id))
                    .map(|o| o.id)
                    .collect();
                for id in mine {
                    if remove_occurrences {
                        self.relationships
                            .retain(|r| r.left_occurrence != id && r.right_occurrence != id);
                        self.occurrences.retain(|o| o.id != id);
                    } else if let Some(o) = self.occurrences.iter_mut().find(|o| o.id == id) {
                        o.table_id = None;
                    }
                }
            }
            SchemaChange::CreateField {
                table_id,
                name,
                data_type,
                comment,
            } => {
                let table = self.table_mut(table_id)?;
                unique_name(&name, table.fields.iter().map(|f| f.name.as_str()))?;
                let id = table
                    .fields
                    .iter()
                    .map(|f| f.id)
                    .max()
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or_else(|| DataError::new("No field IDs are available."))?;
                table.fields.push(SchemaField {
                    id,
                    name: name.trim().to_owned(),
                    kind: SchemaFieldKind::Normal,
                    data_type,
                    comment,
                    annotation: String::new(),
                    display_names_enabled: false,
                    unique: false,
                    unstored: false,
                    options: Some(FieldOptions::default()),
                    calculation: None,
                    summary: None,
                    definition: None,
                });
            }
            SchemaChange::CreateCalculationField {
                table_id,
                name,
                comment,
                calculation,
            } => {
                calculation.check().map_err(DataError::new)?;
                let table = self.table_mut(table_id)?;
                unique_name(&name, table.fields.iter().map(|f| f.name.as_str()))?;
                let id = table
                    .fields
                    .iter()
                    .map(|f| f.id)
                    .max()
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or_else(|| DataError::new("No field IDs are available."))?;
                table.fields.push(SchemaField {
                    id,
                    name: name.trim().to_owned(),
                    kind: SchemaFieldKind::Calculation,
                    data_type: calculation.result,
                    comment,
                    annotation: String::new(),
                    display_names_enabled: false,
                    unique: false,
                    unstored: !calculation.stored(),
                    options: None,
                    calculation: Some(calculation),
                    summary: None,
                    definition: None,
                });
            }
            SchemaChange::CreateSummaryField {
                table_id,
                name,
                comment,
                summary,
            } => {
                let table = self.table_mut(table_id)?;
                unique_name(&name, table.fields.iter().map(|f| f.name.as_str()))?;
                let data_type = crate::summary_field::result_type(&summary, &table.fields)
                    .map_err(DataError::new)?;
                let id = table
                    .fields
                    .iter()
                    .map(|f| f.id)
                    .max()
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or_else(|| DataError::new("No field IDs are available."))?;
                table.fields.push(SchemaField {
                    id,
                    name: name.trim().to_owned(),
                    kind: SchemaFieldKind::Summary,
                    data_type,
                    comment,
                    annotation: String::new(),
                    display_names_enabled: false,
                    unique: false,
                    unstored: false,
                    options: None,
                    calculation: None,
                    summary: Some(summary),
                    definition: None,
                });
            }
            SchemaChange::ChangeSummary {
                table_id,
                field_id,
                summary,
            } => {
                let table = self.table_mut(table_id)?;
                let data_type = crate::summary_field::result_type(&summary, &table.fields)
                    .map_err(DataError::new)?;
                let field = table
                    .fields
                    .iter_mut()
                    .find(|f| f.id == field_id)
                    .ok_or_else(|| DataError::new("The field no longer exists."))?;
                if field.kind != SchemaFieldKind::Summary {
                    return Err(DataError::new("The field is not a summary field."));
                }
                field.data_type = data_type;
                field.summary = Some(summary);
            }
            SchemaChange::ChangeCalculation {
                table_id,
                field_id,
                calculation,
            } => {
                calculation.check().map_err(DataError::new)?;
                let field = self
                    .table_mut(table_id)?
                    .fields
                    .iter_mut()
                    .find(|f| f.id == field_id)
                    .ok_or_else(|| DataError::new("The field no longer exists."))?;
                if field.kind != SchemaFieldKind::Calculation {
                    return Err(DataError::new("The field is not a calculation field."));
                }
                field.data_type = calculation.result;
                field.unstored = !calculation.stored();
                field.calculation = Some(calculation);
            }
            SchemaChange::ChangeField {
                table_id,
                field_id,
                name,
                data_type,
                comment,
            } => {
                let table = self.table_mut(table_id)?;
                unique_name(
                    &name,
                    table
                        .fields
                        .iter()
                        .filter(|f| f.id != field_id)
                        .map(|f| f.name.as_str()),
                )?;
                let field = table
                    .fields
                    .iter_mut()
                    .find(|f| f.id == field_id)
                    .ok_or_else(|| DataError::new("The field no longer exists."))?;
                if field.kind != SchemaFieldKind::Normal && field.data_type != data_type {
                    return Err(DataError::new(
                        "Calculated and summary field definitions require their expression editor.",
                    ));
                }
                field.name = name.trim().to_owned();
                field.data_type = data_type;
                field.comment = comment;
            }
            SchemaChange::DeleteField { table_id, field_id } => {
                let table = self.table_mut(table_id)?;
                if !table.fields.iter().any(|f| f.id == field_id) {
                    return Err(DataError::new("The field no longer exists."));
                }
                table.fields.retain(|f| f.id != field_id);
            }
            SchemaChange::DuplicateField { table_id, field_id } => {
                let table = self.table_mut(table_id)?;
                let original = table
                    .fields
                    .iter()
                    .find(|f| f.id == field_id)
                    .cloned()
                    .ok_or_else(|| DataError::new("The field no longer exists."))?;
                let name = duplicate_name(&original.name, |n| {
                    table.fields.iter().any(|f| f.name.eq_ignore_ascii_case(n))
                })
                .ok_or_else(|| DataError::new("No name is left for a copy."))?;
                let id = table
                    .fields
                    .iter()
                    .map(|f| f.id)
                    .max()
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or_else(|| DataError::new("No field IDs are available."))?;
                table.fields.push(SchemaField {
                    id,
                    name,
                    ..original
                });
            }
            SchemaChange::SetFieldOptions {
                table_id,
                field_id,
                options,
            } => {
                let table = self.table_mut(table_id)?;
                let field = table
                    .fields
                    .iter_mut()
                    .find(|f| f.id == field_id)
                    .ok_or_else(|| DataError::new("The field no longer exists."))?;
                if field.kind != SchemaFieldKind::Normal {
                    return Err(DataError::new(
                        "Calculated and summary field definitions require their expression editor.",
                    ));
                }
                field.unique = options.validation.unique
                    || matches!(options.auto_enter.value, AutoEnterValue::Serial(_));
                field.options = Some(*options);
            }
            SchemaChange::CreateOccurrence { table_id, name } => {
                self.table_mut(table_id)?;
                unique_name(&name, self.occurrences.iter().map(|o| o.name.as_str()))?;
                let id = self
                    .occurrences
                    .iter()
                    .map(|o| o.id)
                    .max()
                    .unwrap_or(0xd00001)
                    .checked_add(1)
                    .ok_or_else(|| DataError::new("No occurrence IDs are available."))?;
                // To the right of the graph's top row.
                let right = self
                    .occurrences
                    .iter()
                    .filter_map(|o| o.graph.map(|b| b.x + b.width))
                    .fold(0.0_f32, f32::max);
                self.occurrences.push(SchemaOccurrence {
                    id,
                    name: name.trim().to_owned(),
                    table_id: Some(table_id),
                    graph: Some(OccurrenceBox::new_at(right + 60.0, 20.0)),
                });
            }
            SchemaChange::RenameOccurrence {
                occurrence_id,
                name,
            } => {
                unique_name(
                    &name,
                    self.occurrences
                        .iter()
                        .filter(|o| o.id != occurrence_id)
                        .map(|o| o.name.as_str()),
                )?;
                self.occurrences
                    .iter_mut()
                    .find(|o| o.id == occurrence_id)
                    .ok_or_else(|| DataError::new("The table occurrence no longer exists."))?
                    .name = name.trim().to_owned();
            }
            SchemaChange::DeleteOccurrence { occurrence_id } => {
                if !self.occurrences.iter().any(|o| o.id == occurrence_id) {
                    return Err(DataError::new("The table occurrence no longer exists."));
                }
                // As in FileMaker Pro, its relationships go with it.
                self.relationships.retain(|r| {
                    r.left_occurrence != occurrence_id && r.right_occurrence != occurrence_id
                });
                self.occurrences.retain(|o| o.id != occurrence_id);
            }
            SchemaChange::CreateRelationship {
                left_occurrence,
                right_occurrence,
                matches,
                left,
                right,
            } => {
                for id in [left_occurrence, right_occurrence] {
                    if !self.occurrences.iter().any(|o| o.id == id) {
                        return Err(DataError::new("The table occurrence no longer exists."));
                    }
                }
                if matches.is_empty() {
                    return Err(DataError::new(
                        "A relationship needs at least one pair of fields.",
                    ));
                }
                let id = self.relationships.iter().map(|r| r.id).max().unwrap_or(0) + 1;
                self.relationships.push(SchemaRelationship {
                    id,
                    left_occurrence,
                    right_occurrence,
                    matches: sample_matches(&matches),
                    options: left.bits(false) | right.bits(true),
                    orders: [left.order, right.order],
                });
            }
            SchemaChange::ChangeRelationship {
                relationship_id,
                matches,
                left,
                right,
            } => {
                if matches.is_empty() {
                    return Err(DataError::new(
                        "A relationship needs at least one pair of fields.",
                    ));
                }
                let relationship = self
                    .relationships
                    .iter_mut()
                    .find(|r| r.id == relationship_id)
                    .ok_or_else(|| DataError::new("The relationship no longer exists."))?;
                relationship.matches = sample_matches(&matches);
                relationship.options = left.bits(false) | right.bits(true);
                for (stored, order) in relationship
                    .orders
                    .iter_mut()
                    .zip([left.order, right.order])
                {
                    if order.is_some() {
                        *stored = order;
                    }
                }
            }
            SchemaChange::DeleteRelationship { relationship_id } => {
                if !self.relationships.iter().any(|r| r.id == relationship_id) {
                    return Err(DataError::new("The relationship no longer exists."));
                }
                self.relationships.retain(|r| r.id != relationship_id);
            }
            // The sample graph has no stored boxes to move or colour.
            SchemaChange::ArrangeOccurrence { occurrence_id, .. }
            | SchemaChange::ColorOccurrence { occurrence_id, .. } => {
                if !self.occurrences.iter().any(|o| o.id == occurrence_id) {
                    return Err(DataError::new("The table occurrence no longer exists."));
                }
            }
            SchemaChange::ArrangeOccurrences { frames } => {
                for (occurrence_id, [left, top, width, height]) in frames {
                    if width <= 0 || height <= 0 {
                        return Err(DataError::new(
                            "A table occurrence's box needs a width and a height.",
                        ));
                    }
                    let occurrence = self
                        .occurrences
                        .iter_mut()
                        .find(|o| o.id == occurrence_id)
                        .ok_or_else(|| DataError::new("The table occurrence no longer exists."))?;
                    let graph = occurrence.graph.as_mut().ok_or_else(|| {
                        DataError::new("The table occurrence has no box to arrange.")
                    })?;
                    graph.x = left as f32;
                    graph.y = top as f32;
                    graph.width = width as f32;
                    graph.height = height as f32;
                }
            }
            SchemaChange::DuplicateOccurrences { occurrence_ids } => {
                if occurrence_ids.is_empty() {
                    return Err(DataError::new("Select the table occurrences to duplicate."));
                }
                let mut copies = std::collections::BTreeMap::new();
                for source in &occurrence_ids {
                    if copies.contains_key(source) {
                        continue;
                    }
                    let original = self
                        .occurrences
                        .iter()
                        .find(|o| o.id == *source)
                        .cloned()
                        .ok_or_else(|| DataError::new("The table occurrence no longer exists."))?;
                    let id = self
                        .occurrences
                        .iter()
                        .map(|o| o.id)
                        .max()
                        .unwrap_or(0xd00001)
                        .checked_add(1)
                        .ok_or_else(|| DataError::new("No occurrence IDs are available."))?;
                    let name =
                        unused_name(&original.name, self.occurrences.iter().map(|o| &o.name));
                    let offset = DUPLICATE_OFFSET as f32;
                    self.occurrences.push(SchemaOccurrence {
                        id,
                        name,
                        graph: original.graph.map(|b| OccurrenceBox {
                            x: b.x + offset,
                            y: b.y + offset,
                            ..b
                        }),
                        ..original
                    });
                    copies.insert(*source, id);
                }
                let joined: Vec<SchemaRelationship> = self
                    .relationships
                    .iter()
                    .filter(|r| {
                        copies.contains_key(&r.left_occurrence)
                            && copies.contains_key(&r.right_occurrence)
                    })
                    .cloned()
                    .collect();
                for mut relationship in joined {
                    // A sort order names the occurrence it sorts by: the copy.
                    for order in relationship.orders.iter_mut().flatten() {
                        for entry in &mut order.fields {
                            let references =
                                std::iter::once(&mut entry.field).chain(entry.summary.as_mut());
                            for reference in references {
                                if let Some(id) = reference.occurrence {
                                    reference.occurrence =
                                        Some(*copies.get(&id).ok_or_else(|| {
                                            DataError::new(
                                                "A relationship's sort order names a table \
                                             occurrence that is not duplicated.",
                                            )
                                        })?);
                                }
                            }
                        }
                    }
                    let id = self.relationships.iter().map(|r| r.id).max().unwrap_or(0) + 1;
                    self.relationships.push(SchemaRelationship {
                        id,
                        left_occurrence: copies[&relationship.left_occurrence],
                        right_occurrence: copies[&relationship.right_occurrence],
                        ..relationship
                    });
                }
            }
        }
        Ok(())
    }

    fn table_mut(&mut self, id: u64) -> Result<&mut SchemaTable, DataError> {
        self.tables
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| DataError::new("The table no longer exists."))
    }
}

fn unique_name<'a>(
    name: &str,
    mut existing: impl Iterator<Item = &'a str>,
) -> Result<(), DataError> {
    let name = name.trim();
    if name.is_empty() || name.contains(['\r', '\n', '\0']) {
        return Err(DataError::new("Enter a name without line breaks."));
    }
    if existing.any(|other| other.to_lowercase() == name.to_lowercase()) {
        return Err(DataError::new("That name is already in use."));
    }
    Ok(())
}

fn sample_matches(matches: &[RelationshipMatch]) -> Vec<SchemaMatch> {
    matches
        .iter()
        .map(|m| SchemaMatch {
            left_field: m.left_field,
            right_field: m.right_field,
            comparison: crate::meta::JoinOp::from_code(m.operator)
                .map_or("?", |op| op.symbol())
                .into(),
            operator: m.operator,
        })
        .collect()
}

/// `name`, or the first of "name 2", "name 3", ... that none of `taken`
/// has (compared ignoring case): the occurrence name FileMaker Pro gives a
/// new table when an occurrence already has the table's name (OpenMaker's
/// reading; FileMaker Pro's own choice in that case was not observed).
pub fn unused_name<'a>(name: &str, taken: impl Iterator<Item = &'a String> + Clone) -> String {
    let free = |candidate: &str| {
        !taken
            .clone()
            .any(|t| t.to_lowercase() == candidate.to_lowercase())
    };
    if free(name) {
        return name.to_owned();
    }
    (2..)
        .map(|n| format!("{name} {n}"))
        .find(|candidate| free(candidate))
        .expect("an unused name")
}

pub(crate) fn mock_schema(
    state: &mut Option<DatabaseSchema>,
    change: Option<SchemaChange>,
) -> Result<DatabaseSchema, DataError> {
    let schema = state.get_or_insert_with(|| DatabaseSchema {
        tables: vec![SchemaTable {
            id: 0x8002,
            name: "Contacts".into(),
            comment: String::new(),
            fields: ["First Name", "Last Name", "Email", "Created"]
                .into_iter()
                .enumerate()
                .map(|(i, name)| SchemaField {
                    id: i as u64 + 1,
                    name: name.into(),
                    kind: SchemaFieldKind::Normal,
                    data_type: if i == 3 {
                        FieldKind::Timestamp
                    } else {
                        FieldKind::Text
                    },
                    comment: String::new(),
                    annotation: String::new(),
                    display_names_enabled: false,
                    unique: false,
                    unstored: false,
                    options: Some(FieldOptions::default()),
                    calculation: None,
                    summary: None,
                    definition: None,
                })
                .collect(),
        }],
        occurrences: vec![SchemaOccurrence {
            id: 0xd00002,
            name: "Contacts".into(),
            table_id: Some(0x8002),
            graph: Some(OccurrenceBox::new_at(20.0, 20.0)),
        }],
        relationships_decoded: true,
        capabilities: SchemaCapabilities {
            tables: true,
            fields: true,
            occurrences: true,
            relationships: false,
            unavailable_reason: None,
        },
        ..Default::default()
    });
    if let Some(change) = change {
        schema.apply_sample_change(change)?;
    }
    Ok(schema.clone())
}

/// The name Duplicate gives a copy of field `name`, FileMaker's unique-name
/// rule (schema-path.md 5.14): `name` without spaces at either end, then,
/// while `taken` says a name is used, a name ending in " Copy" gets the
/// number 2 after it, one ending in " Copy" and a number above 0 gets the
/// next number, and any other gets " Copy" appended. So "Name" becomes
/// "Name Copy", then "Name Copy2", "Name Copy3". `None` after 100 tries,
/// where FileMaker leaves this rule.
pub fn duplicate_name(name: &str, taken: impl Fn(&str) -> bool) -> Option<String> {
    const SUFFIX: &str = " Copy";
    let mut candidate = name.trim().to_string();
    for _ in 0..100 {
        if !taken(&candidate) {
            return Some(candidate);
        }
        let numbered = candidate.rfind(SUFFIX).and_then(|at| {
            let end = at + SUFFIX.len();
            let rest = &candidate[end..];
            if rest.is_empty() {
                return Some((end, 2));
            }
            let all_digits = rest.bytes().all(|b| b.is_ascii_digit());
            let number: u32 = rest.parse().ok().filter(|n| all_digits && *n > 0)?;
            Some((end, number.checked_add(1)?))
        });
        match numbered {
            Some((end, number)) => {
                candidate.truncate(end);
                candidate.push_str(&number.to_string());
            }
            None => candidate.push_str(SUFFIX),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The graph's Arrange tools on three boxes (#430): edges to the
    /// outermost, centers to the bounds' center, the middle box spaced
    /// evenly between the outer two, sizes to the smallest or largest.
    #[test]
    fn arrange_moves_and_resizes_the_selected_boxes() {
        let frames = [
            (1, [10.0, 20.0, 100.0, 50.0]),
            (2, [200.0, 80.0, 60.0, 120.0]),
            (3, [400.0, 40.0, 80.0, 30.0]),
        ];
        let run = |how| match arrange(&frames, how).unwrap() {
            SchemaChange::ArrangeOccurrences { frames } => {
                let mut frames = frames;
                frames.sort();
                frames.into_iter().map(|(_, f)| f).collect::<Vec<_>>()
            }
            other => panic!("{other:?}"),
        };
        use Arrangement::*;
        for (how, expected) in [
            (
                AlignLeft,
                [[10, 20, 100, 50], [10, 80, 60, 120], [10, 40, 80, 30]],
            ),
            (
                AlignRight,
                [[380, 20, 100, 50], [420, 80, 60, 120], [400, 40, 80, 30]],
            ),
            (
                AlignCenter,
                [[195, 20, 100, 50], [215, 80, 60, 120], [205, 40, 80, 30]],
            ),
            (
                AlignTop,
                [[10, 20, 100, 50], [200, 20, 60, 120], [400, 20, 80, 30]],
            ),
            (
                AlignBottom,
                [[10, 150, 100, 50], [200, 80, 60, 120], [400, 170, 80, 30]],
            ),
            (
                AlignMiddle,
                [[10, 85, 100, 50], [200, 50, 60, 120], [400, 95, 80, 30]],
            ),
            (
                DistributeHorizontally,
                [[10, 20, 100, 50], [225, 80, 60, 120], [400, 40, 80, 30]],
            ),
            (
                Resize {
                    largest: false,
                    width: true,
                    height: false,
                },
                [[10, 20, 60, 50], [200, 80, 60, 120], [400, 40, 60, 30]],
            ),
            (
                Resize {
                    largest: true,
                    width: true,
                    height: true,
                },
                [[10, 20, 100, 120], [200, 80, 100, 120], [400, 40, 100, 120]],
            ),
        ] {
            assert_eq!(run(how), expected, "{how:?}");
        }
        assert_eq!(arrange(&frames[..1], AlignLeft).unwrap_err().code, Some(3));
        // A place no whole point holds, or none at all, is not arranged.
        for x in [1.0e30, f32::NAN] {
            let far = [(1, [x, 0.0, 10.0, 10.0]), (2, [0.0, 0.0, 10.0, 10.0])];
            assert_eq!(arrange(&far, AlignTop).unwrap_err().code, Some(3));
        }
    }

    /// The Fields tab's Options text (client-ui.md): a starter's primary
    /// key from its decoded options, a calculation named from its context
    /// once the table has two occurrences, a running total restarted by a
    /// break field, and fields whose stored bits the options cannot hold.
    #[test]
    fn options_read_as_the_fields_tab_lists_them() {
        use crate::calc_field::CalculationField;
        use crate::meta::{summary_flag, SummaryDef, SummaryField, SummaryKind};
        let definition = |hex: &str| -> Vec<u8> {
            let hex: String = hex.split_whitespace().collect();
            (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect()
        };
        let options = FieldOptions::decode(
            &definition("01 01 00 00 00000015 80000188 0000001c 00000001 00000000 0001"),
            None,
            None,
        )
        .unwrap();
        let field = |id: u64, name: &str, kind: SchemaFieldKind| SchemaField {
            id,
            name: name.into(),
            kind,
            data_type: FieldKind::Number,
            comment: String::new(),
            annotation: String::new(),
            display_names_enabled: false,
            unique: false,
            unstored: false,
            options: None,
            calculation: None,
            summary: None,
            definition: None,
        };
        let mut key = field(1, "PrimaryKey", SchemaFieldKind::Normal);
        key.options = Some(options);
        let mut total = field(2, "Total", SchemaFieldKind::Summary);
        let mut summary = SummaryDef::new(SummaryKind::Total, 4);
        summary.flags = summary_flag::RUNNING | summary_flag::RESTART;
        summary.additional = Some(SummaryField {
            occurrence: None,
            field: 5,
            repetition: 1,
        });
        total.summary = Some(summary);
        let mut doubled = field(3, "Doubled", SchemaFieldKind::Calculation);
        doubled.calculation = Some(CalculationField::new("Price\r* 2", 10));
        let table = SchemaTable {
            id: 7,
            name: "Items".into(),
            comment: String::new(),
            fields: vec![
                key,
                total,
                doubled,
                field(4, "Price", SchemaFieldKind::Normal),
                field(5, "Group", SchemaFieldKind::Normal),
            ],
        };
        let occurrence = |id: u64, name: &str| SchemaOccurrence {
            id,
            name: name.into(),
            table_id: Some(7),
            graph: None,
        };
        let mut schema = DatabaseSchema {
            tables: vec![table],
            occurrences: vec![occurrence(10, "Items")],
            ..DatabaseSchema::default()
        };
        let text = |schema: &DatabaseSchema, id: u64| {
            let table = &schema.tables[0];
            let field = table.fields.iter().find(|f| f.id == id).unwrap();
            schema.field_column_text(table, field, FieldColumn::Options)
        };
        assert_eq!(
            text(&schema, 1),
            "Indexed, Auto-enter Calculation replaces existing value, Can't Modify Auto, \
             Required Value, Unique"
        );
        assert_eq!(
            text(&schema, 2),
            "= Total of Price (running with restart), when sorted by Group"
        );
        assert_eq!(text(&schema, 3), "= Price * 2");
        schema.occurrences.push(occurrence(11, "Items 2"));
        assert_eq!(text(&schema, 3), "from Items, = Price * 2");
        // From the stored bits, which the dialog's options cannot all
        // hold: two auto-enter options at once, "Evaluate Always" alone,
        // a relaxed index alone.
        for (options, expected) in [
            ("00000005", "Creation Date, Auto-enter Data"),
            ("00000408", "Auto-enter Calculation, Lookup"),
            ("00000020", "Evaluate Always"),
            ("10000000", "Indexed"),
        ] {
            let table = &mut schema.tables[0];
            table.fields[3].definition = Some(definition(&format!(
                "01 01 00 00 00000015 {options} 00000000 00000001 00000000 0001"
            )));
            assert_eq!(text(&schema, 4), expected, "{options}");
        }
    }

    /// FileMaker's unique-name rule for Duplicate (schema-path.md 5.14).
    #[test]
    fn duplicates_are_named_as_filemaker_names_them() {
        let names = |taken: &[&str], name: &str| {
            duplicate_name(name, |n| taken.iter().any(|t| t.eq_ignore_ascii_case(n)))
        };
        for (taken, name, copy) in [
            (&["Name"][..], "Name", "Name Copy"),
            (&["Name", "Name Copy"], "Name", "Name Copy2"),
            (&["Name", "Name Copy", "Name Copy2"], "Name", "Name Copy3"),
            (&["Name Copy"], "Name Copy", "Name Copy2"),
            (&["Name Copy9"], "Name Copy9", "Name Copy10"),
            (&["Copy Count"], "Copy Count", "Copy Count Copy"),
            (&["Name Copy 2"], "Name Copy 2", "Name Copy 2 Copy"),
            // 0 reads as no number (`ExtractTrailingNumber`).
            (&["Name Copy0"], "Name Copy0", "Name Copy0 Copy"),
            (&[" Name ", "Name"], " Name ", "Name Copy"),
        ] {
            assert_eq!(names(taken, name).as_deref(), Some(copy), "{taken:?}");
        }
    }

    #[test]
    fn edits_preserve_ids_and_invalid_changes_are_atomic() {
        let mut state = None;
        let mut schema = mock_schema(&mut state, None).unwrap();
        schema
            .apply_sample_change(SchemaChange::ChangeField {
                table_id: 0x8002,
                field_id: 1,
                name: "Given Name".into(),
                data_type: FieldKind::Text,
                comment: String::new(),
            })
            .unwrap();
        assert_eq!(schema.tables[0].fields[0].id, 1);
        assert_eq!(schema.tables[0].fields[0].name, "Given Name");
        let before = schema.clone();
        assert!(schema
            .apply_sample_change(SchemaChange::CreateField {
                table_id: 0x8002,
                name: "given name".into(),
                data_type: FieldKind::Text,
                comment: String::new(),
            })
            .is_err());
        assert_eq!(schema, before);
        assert!(schema
            .apply_sample_change(SchemaChange::DeleteTable {
                table_id: 0x9999,
                remove_occurrences: false,
            })
            .is_err());
        assert_eq!(schema, before);
    }
    #[test]
    fn table_field_and_occurrence_lifecycles_are_real_sample_catalog_edits() {
        let mut schema = DatabaseSchema::default();
        schema
            .apply_sample_change(SchemaChange::CreateTable {
                name: "Orders".into(),
                comment: String::new(),
            })
            .unwrap();
        let table_id = schema.tables[0].id;
        // The new table's own occurrence.
        assert_eq!(schema.occurrences[0].name, "Orders");
        schema
            .apply_sample_change(SchemaChange::CreateField {
                table_id,
                name: "Total".into(),
                data_type: FieldKind::Number,
                comment: String::new(),
            })
            .unwrap();
        let field_id = schema.tables[0].fields[0].id;
        schema
            .apply_sample_change(SchemaChange::CreateOccurrence {
                table_id,
                name: "Current Orders".into(),
            })
            .unwrap();
        let occurrence_id = schema.occurrences[1].id;
        schema
            .apply_sample_change(SchemaChange::RenameOccurrence {
                occurrence_id,
                name: "Orders by Customer".into(),
            })
            .unwrap();
        schema
            .apply_sample_change(SchemaChange::ChangeTable {
                table_id,
                name: "Sales".into(),
                comment: "Orders taken".into(),
            })
            .unwrap();
        assert_eq!(schema.tables[0].name, "Sales");
        // The occurrence named as the table follows it; the other does not.
        assert_eq!(schema.occurrences[0].name, "Sales");
        assert_eq!(schema.occurrences[1].name, "Orders by Customer");
        assert_eq!(schema.occurrences[1].table_id, Some(table_id));
        schema
            .apply_sample_change(SchemaChange::DeleteField { table_id, field_id })
            .unwrap();
        schema
            .apply_sample_change(SchemaChange::DeleteOccurrence { occurrence_id })
            .unwrap();
        schema
            .apply_sample_change(SchemaChange::DeleteTable {
                table_id,
                remove_occurrences: false,
            })
            .unwrap();
        assert!(schema.tables.is_empty());
        // Its other occurrence stays, showing a missing table.
        assert_eq!(schema.occurrences.len(), 1);
        assert_eq!(schema.occurrences[0].table_id, None);
    }
}
