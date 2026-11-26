//! Layout definition and parts: `FMLayoutPB.LayoutDef` and `PartDef`
//! (`FMLayout.proto`; semantics in layout-model.md 4).

use crate::object::Rect;
use crate::style::{ObjectStyle, ThemeStyles};
use crate::wire::{Fields, Merged, RawField, Result, Value};

/// A layout's Layout Setup word, its flags `f1` or its member flags `f0`,
/// from its catalog member's copy (`[4, 5, L]`), else its catalog index
/// entry's (`[4, 1, 7, L]`): FileMaker keeps both, and a reader that took
/// the member's alone would drop what only the entry holds (#977). Each copy
/// is its slot's own value, a big-endian unsigned integer of 1 to 8 bytes
/// (not a split value). The one reading rule for every reader and the
/// host's Layout Setup
/// (`Layout::decode_from_tree`, `om_data::layout_catalog::layout_word`).
pub fn stored_layout_word(member: Option<&[u8]>, entry: Option<&[u8]>) -> Option<u64> {
    member
        .and_then(om_hbam::slot::be_uint)
        .or_else(|| entry.and_then(om_hbam::slot::be_uint))
}

/// `FMLayoutPB.LayoutDef`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayoutDef {
    /// Effective view enable bits and default view (layout-model.md 4.2).
    /// `Layout::decode_from_tree` hydrates these from member key 0xf1; a
    /// standalone protobuf decode retains its raw field-1 value.
    pub flags: u64,
    /// `tableAliasKey`: the table occurrence the layout shows records from.
    pub table_alias_key: u64,
    pub menu_set_key: u64,
    pub local_styles: Option<ThemeStyles>,
    pub background_style: Option<ObjectStyle>,
    pub box_style: Option<ObjectStyle>,
    pub table_grid_style: Option<ObjectStyle>,
    /// `baseWidth`: the layout width in points.
    pub base_width: f64,
    pub max_width: f64,
    pub print_column_width: f64,
    pub print_column_count: u32,
    /// `customMargins`: fixed print margins.
    pub custom_margins: Option<Rect>,
    /// `tblCustomRowHeight`: table view row height.
    pub table_row_height: f64,
    /// `tblHeaderSize`: table view header size.
    pub table_header_size: f64,
    pub layout_type: u32,
    pub orientation: u32,
    /// 0 or 3 non-responsive, 1 not set, 2 responsive.
    pub layout_client_type: u32,
    /// Unknown field numbers and fields with an unexpected wire type,
    /// preserved in wire order.
    pub other: Vec<RawField>,
}

/// A layout's `tableAliasKey` (and a field spec's `tableKey`) as the
/// catalog id of the table occurrence: the serial key segment of the value
/// read as a number, as the native provider maps it.
pub fn occurrence_id(alias: u64) -> Option<u64> {
    om_hbam::slot::be_uint(&om_hbam::serial::encode(alias))
}

impl LayoutDef {
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut def = LayoutDef::default();
        let mut margins = Merged::default();
        let mut local_styles = Merged::default();
        let mut background = Merged::default();
        let mut box_style = Merged::default();
        let mut grid = Merged::default();
        for field in Fields::new(buf, "LayoutDef") {
            let field = field?;
            match (field.number, field.value) {
                (1, Value::Varint(v)) => def.flags = v,
                (2, Value::Varint(v)) => def.table_alias_key = v,
                (3, Value::Varint(v)) => def.menu_set_key = v,
                (4, Value::Bytes(b)) => local_styles.push(b),
                (5, Value::Bytes(b)) => background.push(b),
                (6, Value::Bytes(b)) => box_style.push(b),
                (7, Value::Bytes(b)) => grid.push(b),
                (8, Value::Fixed64(v)) => def.base_width = f64::from_bits(v),
                (9, Value::Fixed64(v)) => def.max_width = f64::from_bits(v),
                (10, Value::Fixed64(v)) => def.print_column_width = f64::from_bits(v),
                (11, Value::Varint(v)) => def.print_column_count = v as u32,
                (12, Value::Bytes(b)) => margins.push(b),
                (13, Value::Fixed64(v)) => def.table_row_height = f64::from_bits(v),
                (14, Value::Fixed64(v)) => def.table_header_size = f64::from_bits(v),
                (15, Value::Varint(v)) => def.layout_type = v as u32,
                (16, Value::Varint(v)) => def.orientation = v as u32,
                (17, Value::Varint(v)) => def.layout_client_type = v as u32,
                _ => def.other.push(field.into()),
            }
        }
        def.custom_margins = margins.decode(Rect::decode)?;
        def.local_styles = local_styles.decode(ThemeStyles::decode)?;
        def.background_style = background.decode(ObjectStyle::decode)?;
        def.box_style = box_style.decode(ObjectStyle::decode)?;
        def.table_grid_style = grid.decode(ObjectStyle::decode)?;
        Ok(def)
    }

    /// Layout Setup's "Enable Quick Find": flag `0x8000` set turns Quick
    /// Find off for the layout (layout-model.md 4.2).
    pub fn quick_find(&self) -> bool {
        self.flags & 0x8000 == 0
    }
}

/// Part kind from `PartDef.type` (layout-model.md 4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PartKind {
    TitleHeader,
    Header,
    LeadingGrandSummary,
    LeadingSubSummary,
    Body,
    TrailingSubSummary,
    TrailingGrandSummary,
    Footer,
    TitleFooter,
    TopNavigation,
    BottomNavigation,
    /// Any other value (9-11 map to the generic `part` style).
    Other(i32),
}

impl PartKind {
    pub fn from_type(part_type: i32) -> Self {
        match part_type {
            0 => PartKind::TitleHeader,
            1 => PartKind::Header,
            2 => PartKind::LeadingGrandSummary,
            3 => PartKind::LeadingSubSummary,
            4 => PartKind::Body,
            5 => PartKind::TrailingSubSummary,
            6 => PartKind::TrailingGrandSummary,
            7 => PartKind::Footer,
            8 => PartKind::TitleFooter,
            12 => PartKind::TopNavigation,
            13 => PartKind::BottomNavigation,
            other => PartKind::Other(other),
        }
    }
}

/// A sub-summary part's break field in the calculation schema's numbering:
/// the big-endian readings of its occurrence and field key segments (as
/// [`crate::occurrence_id`] and `FieldSpec::field_id` give them).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BreakField {
    pub occurrence: u64,
    pub field: u64,
}

/// A layout part: its catalog key and `FMLayoutPB.PartDef`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Part {
    /// Part key (`4/5/<layout>/3/<id>`).
    pub id: u64,
    /// `type`, see [`Part::kind`].
    pub part_type: i32,
    pub flags: u64,
    pub page_break_after: u32,
    /// `offset`: top edge in layout coordinates (points).
    pub top: f64,
    /// `size`: height in points.
    pub height: f64,
    /// `breakFieldKey` (opaque; the sub-summary break field, inferred).
    pub break_field_key: Vec<u8>,
    pub style: Option<ObjectStyle>,
    pub alt_style: Option<ObjectStyle>,
    /// Unknown field numbers and fields with an unexpected wire type,
    /// preserved in wire order.
    pub other: Vec<RawField>,
}

impl Part {
    /// Decode the `PartDef` of part `id`.
    pub fn decode(id: u64, part_def: &[u8]) -> Result<Self> {
        let mut part = Part {
            id,
            ..Part::default()
        };
        let mut style = Merged::default();
        let mut alt_style = Merged::default();
        for field in Fields::new(part_def, "PartDef") {
            let field = field?;
            match (field.number, field.value) {
                (1, Value::Varint(v)) => part.part_type = v as i32,
                (2, Value::Varint(v)) => part.flags = v,
                (3, Value::Varint(v)) => part.page_break_after = v as u32,
                (4, Value::Fixed64(v)) => part.top = f64::from_bits(v),
                (5, Value::Fixed64(v)) => part.height = f64::from_bits(v),
                (6, Value::Bytes(b)) => part.break_field_key = b.to_vec(),
                (10, Value::Bytes(b)) => style.push(b),
                (11, Value::Bytes(b)) => alt_style.push(b),
                _ => part.other.push(field.into()),
            }
        }
        part.style = style.decode(ObjectStyle::decode)?;
        part.alt_style = alt_style.decode(ObjectStyle::decode)?;
        Ok(part)
    }

    pub fn kind(&self) -> PartKind {
        PartKind::from_type(self.part_type)
    }

    /// The "when sorted by" field of a sub-summary part; `Ok(None)` without
    /// a key. `breakFieldKey` is a counted key holding a counted segment
    /// list `[n][n - 1][file][occurrence][field]`, each segment
    /// length-prefixed, with an empty file segment in every template seen
    /// (layout-model.md 4.3 [I]). Another file, extra segments or any other
    /// shape is an error rather than a guess.
    pub fn break_field(&self) -> std::result::Result<Option<BreakField>, String> {
        let key = self.break_field_key.as_slice();
        if key.is_empty() {
            return Ok(None);
        }
        let invalid = || {
            format!(
                "Unrecognized sub-summary break field key in part {}",
                self.id
            )
        };
        let counted = |bytes: &[u8]| -> Option<Vec<u8>> {
            let (&n, rest) = bytes.split_first()?;
            (usize::from(n) == rest.len()).then(|| rest.to_vec())
        };
        let list = counted(key)
            .and_then(|inner| counted(&inner))
            .ok_or_else(invalid)?;
        let mut segments = Vec::new();
        let mut rest = list.as_slice();
        while let Some((&len, tail)) = rest.split_first() {
            let segment = tail.get(..usize::from(len)).ok_or_else(invalid)?;
            segments.push(segment);
            rest = &tail[segment.len()..];
        }
        match segments[..] {
            [[], occurrence, field] => {
                let id = |segment: &[u8]| om_hbam::slot::be_uint(segment).filter(|id| *id != 0);
                Ok(Some(BreakField {
                    occurrence: id(occurrence).ok_or_else(invalid)?,
                    field: id(field).ok_or_else(invalid)?,
                }))
            }
            _ => Err(invalid()),
        }
    }

    /// Bottom edge in layout coordinates.
    pub fn bottom(&self) -> f64 {
        self.top + self.height
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_def() {
        // Top navigation part 0/44 with a style, as in the Tasks template.
        let mut buf = vec![0x08, 12];
        buf.push(0x21);
        buf.extend_from_slice(&0f64.to_le_bytes());
        buf.push(0x29);
        buf.extend_from_slice(&44f64.to_le_bytes());
        buf.extend_from_slice(&[0x52, 0x02, 0x18, 40]);
        let part = Part::decode(3, &buf).unwrap();
        assert_eq!(part.kind(), PartKind::TopNavigation);
        assert_eq!(
            (part.id, part.top, part.height, part.bottom()),
            (3, 0.0, 44.0, 44.0)
        );
        assert!(part.other.is_empty());
        assert_eq!(part.style.unwrap().object_type_id, 40);
    }

    #[test]
    fn sub_summary_break_field() {
        // Leading sub-summary 74/30 by field 4 of occurrence d0 00 02, as
        // in the Tasks template.
        let mut buf = vec![0x08, 3];
        buf.push(0x21);
        buf.extend_from_slice(&74f64.to_le_bytes());
        buf.push(0x29);
        buf.extend_from_slice(&30f64.to_le_bytes());
        let key = [0x08, 0x07, 0x00, 0x03, 0xd0, 0x00, 0x02, 0x01, 0x04];
        buf.extend_from_slice(&[0x32, key.len() as u8]);
        buf.extend_from_slice(&key);
        let mut part = Part::decode(5, &buf).unwrap();
        assert_eq!(part.kind(), PartKind::LeadingSubSummary);
        assert_eq!(
            part.break_field(),
            Ok(Some(BreakField {
                occurrence: 0xd0_0002,
                field: 4
            }))
        );
        for bad in [
            &[0x08, 0x07, 0x01, 0x05, 0x03, 0xd0, 0x00, 0x02, 0x01][..],
            &[0x07, 0x07, 0x00, 0x03, 0xd0, 0x00, 0x02, 0x01, 0x04],
            &[0x06, 0x05, 0x00, 0x03, 0xd0, 0x00, 0x02],
            &[0x05, 0x04, 0x00, 0x01, 0x03, 0x00],
        ] {
            part.break_field_key = bad.to_vec();
            assert!(part.break_field().is_err(), "{bad:02x?}");
        }
        part.break_field_key.clear();
        assert_eq!(part.break_field(), Ok(None));
    }

    #[test]
    fn layout_def() {
        let mut buf = vec![0x10, 0x82, 0x81, 0x41];
        buf.push(0x41);
        buf.extend_from_slice(&1024f64.to_le_bytes());
        let mut margins = vec![0x09];
        margins.extend_from_slice(&36f64.to_le_bytes());
        buf.push(0x62);
        buf.push(margins.len() as u8);
        buf.extend_from_slice(&margins);
        buf.extend_from_slice(&[0x88, 0x01, 2]);
        let def = LayoutDef::decode(&buf).unwrap();
        assert_eq!(def.table_alias_key, 1065090);
        assert_eq!(def.base_width, 1024.0);
        assert_eq!(def.custom_margins.unwrap().top, 36.0);
        assert_eq!(def.layout_client_type, 2);
        assert!(def.other.is_empty());
    }
}
