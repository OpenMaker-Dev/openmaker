//! Layout objects: `FMLayoutPB.LayoutObjectStore` and the `LayoutObject`
//! messages it holds (`LayoutObject.proto`; semantics in layout-model.md 5).

use std::collections::HashMap;

use crate::calcs::ObjectCalcs;
use crate::style::ObjectStyle;
use crate::wire::{push_varints, Fields, Merged, RawField, Result, Value};

/// `FMLayoutPB.Rect`: edges in points (layout-model.md 3).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rect {
    pub top: f64,
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
    /// Fields not decoded into the members above, verbatim in wire order.
    pub other: Vec<RawField>,
}

impl Rect {
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut rect = Rect::default();
        for field in Fields::new(buf, "Rect") {
            let field = field?;
            match (field.number, field.value) {
                (1, Value::Fixed64(v)) => rect.top = f64::from_bits(v),
                (2, Value::Fixed64(v)) => rect.left = f64::from_bits(v),
                (3, Value::Fixed64(v)) => rect.bottom = f64::from_bits(v),
                (4, Value::Fixed64(v)) => rect.right = f64::from_bits(v),
                _ => rect.other.push(field.into()),
            }
        }
        Ok(rect)
    }

    pub fn width(&self) -> f64 {
        self.right - self.left
    }

    pub fn height(&self) -> f64 {
        self.bottom - self.top
    }
}

/// Object kind from the wire `LayoutObject.type` (layout-model.md 5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    /// Field, in any control style (edit box, pop-up, checkbox set, ...).
    Field,
    Text,
    /// Static image.
    Graphic,
    Line,
    Rectangle,
    RoundedRectangle,
    Oval,
    /// Group, also a grouped button.
    Group,
    Portal,
    /// Button, also each button bar segment.
    Button,
    TabControl,
    TabPanel,
    /// Web viewer, chart, or plug-in object.
    External,
    PopoverButton,
    Popover,
    SlideControl,
    SlidePanel,
    ButtonBar,
    /// A type this crate does not know (FileMaker creates
    /// `UnknownLayoutObject` for these).
    Unknown(u32),
}

impl ObjectKind {
    pub fn from_type(object_type: u32) -> Self {
        match object_type {
            1 => ObjectKind::Field,
            2 => ObjectKind::Text,
            3 => ObjectKind::Graphic,
            4 => ObjectKind::Line,
            5 => ObjectKind::Rectangle,
            6 => ObjectKind::RoundedRectangle,
            7 => ObjectKind::Oval,
            8 => ObjectKind::Group,
            9 => ObjectKind::Portal,
            10 => ObjectKind::Button,
            11 => ObjectKind::TabControl,
            12 => ObjectKind::TabPanel,
            13 => ObjectKind::External,
            14 => ObjectKind::PopoverButton,
            15 => ObjectKind::Popover,
            16 => ObjectKind::SlideControl,
            17 => ObjectKind::SlidePanel,
            18 => ObjectKind::ButtonBar,
            other => ObjectKind::Unknown(other),
        }
    }
}

/// `FMLayoutPB.FieldSpec`: the field a field object shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FieldSpec {
    /// `tableKey`: the serial value of the occurrence's key, in the same
    /// number space as `LayoutDef.tableAliasKey`. [`crate::occurrence_id`]
    /// converts it to the catalog's big-endian key reading.
    pub table_key: u64,
    /// `fieldKey`: the serial value of the field's key within the occurrence's
    /// base table. Use [`Self::field_id`] to look it up in the catalog.
    pub field_key: u64,
    /// `repetition`: the first repetition shown, 1-based (inferred).
    pub repetition: i32,
    /// Fields not decoded into the members above, verbatim in wire order.
    pub other: Vec<RawField>,
}

impl FieldSpec {
    /// The field's catalog id as the schema keys it: `field_key` is the
    /// serial value of the field's key (file-metadata.md 1), equal to the
    /// id below 128.
    pub fn field_id(&self) -> Option<u64> {
        om_hbam::serial::raw_of_id(self.field_key)
    }

    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut spec = FieldSpec::default();
        for field in Fields::new(buf, "FieldSpec") {
            let field = field?;
            match (field.number, field.value) {
                (1, Value::Varint(v)) => spec.table_key = v,
                (2, Value::Varint(v)) => spec.field_key = v,
                (3, Value::Varint(v)) => spec.repetition = v as i32,
                _ => spec.other.push(field.into()),
            }
        }
        Ok(spec)
    }
}

/// A field object's "Field entry behavior" in Browse or Find mode
/// (Inspector, Data tab; FileMaker Pro Help, "Allowing or preventing entry
/// into fields"), as FMEngine's `FieldLayoutObject::GetFieldEntryMode`
/// numbers it (layout-model.md 5.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldEntry {
    /// Users can modify the field's data (the default).
    Edit,
    /// Users can select and copy the data but not change it.
    SelectOnly,
    /// No entry: the data is only viewed.
    ViewOnly,
    /// Set by a field entry calculation (`KFieldEntryCalc`).
    Calculated,
}

/// The `FieldLayoutObject.flags` bit pairs of [`FieldEntry`]: a low bit
/// that clients before 26.0 read alone as "no entry", and a high bit
/// (`FieldLayoutObject::SetFieldEntryMode`'s tables: Select Only sets the
/// high bit, View Only the low one, a calculation both).
const BROWSE_ENTRY: (u32, u32) = (0x4, 0x100_0000);
const FIND_ENTRY: (u32, u32) = (0x10, 0x200_0000);

/// `FMLayoutPB.FieldLayoutObject` (`LayoutObject.fieldObj`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FieldObject {
    pub flags: u32,
    pub field_spec: Option<FieldSpec>,
    /// `numOfReps`: repetitions shown.
    pub num_of_reps: i32,
    pub input_mode: u32,
    /// `displayType`: control style (layout-model.md 5.4).
    pub display_type: u32,
    pub value_list_key: u64,
    pub keyboard_type: u32,
    pub scrollbar_style: u32,
    /// Fields not decoded into the members above, verbatim in wire order.
    pub other: Vec<RawField>,
}

impl FieldObject {
    /// The object's field entry behavior in Find mode (`find`) or Browse
    /// mode.
    pub fn field_entry(&self, find: bool) -> FieldEntry {
        let (low, high) = if find { FIND_ENTRY } else { BROWSE_ENTRY };
        match (self.flags & low != 0, self.flags & high != 0) {
            (false, false) => FieldEntry::Edit,
            (false, true) => FieldEntry::SelectOnly,
            (true, false) => FieldEntry::ViewOnly,
            (true, true) => FieldEntry::Calculated,
        }
    }

    /// The Inspector's "Include field for Quick Find": flag `0x8000` set
    /// leaves the field out (layout-model.md 5.4.1).
    pub fn quick_find(&self) -> bool {
        self.flags & 0x8000 == 0
    }

    /// The Inspector's "Auto-complete using value list": flag `0x800`
    /// (layout-model.md 5.4.1). A drop-down list completes the text typed
    /// from its items.
    pub fn auto_completes(&self) -> bool {
        self.flags & 0x800 != 0
    }

    /// Whether Tab into the field in Browse selects its whole content, so
    /// typing replaces it, rather than putting the insertion point after
    /// it. FileMaker Pro 26.0.3 selects a drop-down calendar's date (#964,
    /// Meetings' Date) and not an edit box's text (#848, Contacts' Website).
    /// That the control, not the date or its auto-entry, decides is
    /// inferred (client-ui.md, "Tab in Browse").
    pub fn tab_selects_content(&self) -> bool {
        self.display_type == 6
    }

    /// Native `FieldLayoutObject::HasIcon`: drop-down/calendar arrows are
    /// opt-in; pop-up menus can hide theirs with flag 0x80000.
    pub fn has_icon(&self) -> bool {
        match self.display_type {
            1 | 6 => self.flags & 0x400 != 0,
            2 => self.flags & 0x80400 != 0x80000,
            _ => false,
        }
    }

    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut obj = FieldObject::default();
        let mut field_spec = Merged::default();
        for field in Fields::new(buf, "FieldLayoutObject") {
            let field = field?;
            match (field.number, field.value) {
                (1, Value::Varint(v)) => obj.flags = v as u32,
                (2, Value::Bytes(b)) => field_spec.push(b),
                (3, Value::Varint(v)) => obj.num_of_reps = v as i32,
                (4, Value::Varint(v)) => obj.input_mode = v as u32,
                (5, Value::Varint(v)) => obj.display_type = v as u32,
                (6, Value::Varint(v)) => obj.value_list_key = v,
                (7, Value::Varint(v)) => obj.keyboard_type = v as u32,
                (8, Value::Varint(v)) => obj.scrollbar_style = v as u32,
                _ => obj.other.push(field.into()),
            }
        }
        obj.field_spec = field_spec.decode(FieldSpec::decode)?;
        Ok(obj)
    }
}

/// Tab control state. Panel keys reference objects in the same store.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TabObject {
    pub flags: i32,
    pub tab_width: u32,
    pub tab_height: i32,
    pub tab_style: i32,
    pub default_panel_key: u64,
    pub current_panel_key: u64,
    pub other: Vec<RawField>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TabPanelObject {
    pub flags: i32,
    pub tab_left_edge: i32,
    pub tab_width: i32,
    pub text_flags: i32,
    pub other: Vec<RawField>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GraphicObject {
    pub library_key: u64,
    pub pict_format: u32,
    pub mix_color: i32,
    pub other: Vec<RawField>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ButtonObject {
    pub flags: i32,
    pub text_flags: i32,
    pub script_step: Option<ScriptStep>,
    pub library_key: u64,
    pub icon_size: i32,
    pub display_type: i32,
    pub other: Vec<RawField>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PopoverButtonObject {
    pub flags: u32,
    pub text_flags: u32,
    pub popover_key: u64,
    pub library_key: u64,
    pub icon_size: i32,
    pub display_type: i32,
    pub other: Vec<RawField>,
}

/// `FMLayoutPB.ExternalLayoutObject` (`LayoutObject.externalObj`): a web
/// viewer, chart or plug-in object (layout-model.md 5.3, 9.6).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExternalObject {
    /// `type`: a four-character code, `WEBV` for a web viewer.
    pub type_code: u32,
    pub sub_type: u32,
    /// `flags`: for a web viewer, its setup options ([`WebViewerOptions`]).
    pub flags: u32,
    /// Fields not decoded into the members above, verbatim in wire order.
    pub other: Vec<RawField>,
}

impl ExternalObject {
    /// The external type of a web viewer.
    pub const WEB_VIEWER: u32 = u32::from_be_bytes(*b"WEBV");

    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut out = ExternalObject::default();
        for field in Fields::new(buf, "ExternalLayoutObject") {
            let field = field?;
            match (field.number, field.value) {
                (1, Value::Varint(v)) => out.type_code = v as u32,
                (2, Value::Varint(v)) => out.sub_type = v as u32,
                (3, Value::Varint(v)) => out.flags = v as u32,
                _ => out.other.push(field.into()),
            }
        }
        Ok(out)
    }
}

/// A web viewer's Web Viewer Setup options: the low bits of its
/// `externalObj.flags`, as FileMaker Pro's `WebViewerSetupModel` setters
/// write them (layout-model.md 9.6). Other bits (`0x8000` is common) are
/// kept but not interpreted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WebViewerOptions(pub u32);

impl WebViewerOptions {
    /// "Allow interaction with web viewer content".
    pub fn allows_interaction(self) -> bool {
        self.0 & 0x1 != 0
    }

    /// "Display content in Find mode".
    pub fn shows_in_find_mode(self) -> bool {
        self.0 & 0x2 != 0
    }

    /// "Display progress bar".
    pub fn shows_progress_bar(self) -> bool {
        self.0 & 0x4 != 0
    }

    /// "Display status messages".
    pub fn shows_status_messages(self) -> bool {
        self.0 & 0x8 != 0
    }

    /// "Automatically encode URL": on unless bit `0x20` is set.
    pub fn encodes_url(self) -> bool {
        self.0 & 0x20 == 0
    }

    /// "Allow JavaScript to perform FileMaker scripts".
    pub fn allows_javascript_scripts(self) -> bool {
        self.0 & 0x40 != 0
    }
}

/// `FMLayoutPB.SegmentedBarLayoutObject` (`LayoutObject.segmentedBarObj`):
/// a button bar. Its segments are its children (buttons or popover
/// buttons, layout-model.md 5.3).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SegmentedBarObject {
    pub flags: u32,
    /// `activeSegmentKey`: the object key of the segment Button Bar Setup
    /// makes active, 0 (absent) for None. The templates' navigation bars
    /// store each layout's own segment here (layout-model.md 5.5).
    pub active_segment_key: u64,
    pub other: Vec<RawField>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PopoverObject {
    pub flags: u32,
    pub text_flags: u32,
    pub position: u32,
    pub button_key: u64,
    pub other: Vec<RawField>,
}

/// A button's stored FileMaker script step. Unknown parameters are retained.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScriptStep {
    pub id: i32,
    pub value: u32,
    pub flags: i32,
    pub reserved: Vec<u8>,
    /// The owning object's attribute 17 parameter subtree. It is stored
    /// beside the protobuf descriptor and hydrated by `decode_from_tree`.
    pub data: Option<om_hbam::OwnedNode>,
    pub other: Vec<RawField>,
}

impl ScriptStep {
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut step = Self::default();
        for field in Fields::new(buf, "ScriptStep") {
            let field = field?;
            match (field.number, field.value) {
                (1, Value::Varint(v)) => step.id = v as i32,
                (2, Value::Varint(v)) => step.value = v as u32,
                (3, Value::Varint(v)) => step.flags = v as i32,
                (4, Value::Bytes(b)) => step.reserved = b.to_vec(),
                _ => step.other.push(field.into()),
            }
        }
        Ok(step)
    }

    /// A local file-catalog reference, as used by Go to Layout and Perform
    /// Script: an `HBKey<5>` of one segment, read as the catalog id. Only the
    /// `1 + length` key bytes are read; the filler after them varies (`be`,
    /// `00`, stale bytes; script-steps.md 2.1). Other encodings stay
    /// unsupported.
    pub fn local_reference(&self) -> Option<u64> {
        let len = usize::from(*self.reserved.first()?);
        let (&seglen, segment) = self.reserved.get(1..1 + len)?.split_first()?;
        if usize::from(seglen) != segment.len() || segment.is_empty() {
            return None;
        }
        om_hbam::slot::be_uint(segment)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupObject {
    pub script_step: Option<ScriptStep>,
    pub other: Vec<RawField>,
}
impl GroupObject {
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut out = Self::default();
        let mut step = Merged::default();
        for field in Fields::new(buf, "GroupLayoutObject") {
            let field = field?;
            match (field.number, field.value) {
                (1, Value::Bytes(b)) => step.push(b),
                _ => out.other.push(field.into()),
            }
        }
        out.script_step = step.decode(ScriptStep::decode)?;
        Ok(out)
    }
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PortalObject {
    pub table_context_key: u64,
    pub table_alias_key: u64,
    pub flags: i32,
    pub initial_row: i32,
    pub num_rows: f64,
    pub scrollbar_style: u32,
    pub other: Vec<RawField>,
}
/// `decode` for a message of varint fields only: each listed field number
/// sets its member, every other field is kept in `other`.
macro_rules! scalar_message {
    ($name:ident, $message:literal, [$($number:literal => $member:ident: $ty:ty),* $(,)?]) => {
        impl $name {
            pub fn decode(buf: &[u8]) -> Result<Self> {
                let mut out = Self::default();
                for field in Fields::new(buf, $message) {
                    let field = field?;
                    match (field.number, field.value) {
                        $(($number, Value::Varint(v)) => out.$member = v as $ty,)*
                        _ => out.other.push(field.into()),
                    }
                }
                Ok(out)
            }
        }
    };
}
scalar_message!(
    TabObject,
    "TabLayoutObject",
    [
        1 => flags: i32,
        2 => tab_width: u32,
        3 => tab_height: i32,
        4 => tab_style: i32,
        5 => default_panel_key: u64,
        6 => current_panel_key: u64,
    ]
);
scalar_message!(
    TabPanelObject,
    "TabPanelLayoutObject",
    [
        1 => flags: i32,
        2 => tab_left_edge: i32,
        3 => tab_width: i32,
        4 => text_flags: i32,
    ]
);
scalar_message!(
    GraphicObject,
    "GraphicLayoutObject",
    [
        1 => library_key: u64,
        2 => pict_format: u32,
        3 => mix_color: i32,
    ]
);
scalar_message!(
    PopoverButtonObject,
    "PopoverButtonLayoutObject",
    [1 => flags: u32, 2 => text_flags: u32, 3 => popover_key: u64,
     4 => library_key: u64, 5 => icon_size: i32, 6 => display_type: i32]
);
scalar_message!(
    PopoverObject,
    "PopoverLayoutObject",
    [1 => flags: u32, 2 => text_flags: u32, 3 => position: u32, 4 => button_key: u64]
);
scalar_message!(
    SegmentedBarObject,
    "SegmentedBarLayoutObject",
    [1 => flags: u32, 2 => active_segment_key: u64]
);
impl ButtonObject {
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut out = Self::default();
        let mut step = Merged::default();
        for field in Fields::new(buf, "ButtonLayoutObject") {
            let field = field?;
            match (field.number, field.value) {
                (1, Value::Varint(v)) => out.flags = v as i32,
                (2, Value::Varint(v)) => out.text_flags = v as i32,
                (3, Value::Bytes(b)) => step.push(b),
                (4, Value::Varint(v)) => out.library_key = v,
                (5, Value::Varint(v)) => out.icon_size = v as i32,
                (6, Value::Varint(v)) => out.display_type = v as i32,
                _ => out.other.push(field.into()),
            }
        }
        out.script_step = step.decode(ScriptStep::decode)?;
        Ok(out)
    }
}
impl PortalObject {
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut out = Self::default();
        for field in Fields::new(buf, "PortalLayoutObject") {
            let field = field?;
            match (field.number, field.value) {
                (1, Value::Varint(v)) => out.table_context_key = v,
                (2, Value::Varint(v)) => out.table_alias_key = v,
                (3, Value::Varint(v)) => out.flags = v as i32,
                (5, Value::Varint(v)) => out.initial_row = v as i32,
                (6, Value::Fixed64(v)) => out.num_rows = f64::from_bits(v),
                (7, Value::Varint(v)) => out.scrollbar_style = v as u32,
                _ => out.other.push(field.into()),
            }
        }
        Ok(out)
    }
}

/// Text content of a rich label; merge fields resolve against the current
/// record's named values. Calculations are not guessed from string fragments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextRun {
    Literal(String),
    MergeField(String),
}

/// `FMLayoutPB.LayoutObject`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayoutObject {
    /// `key`: unique within the layout (a per-layout serial number).
    pub id: u64,
    /// `type`: the wire object type, see [`LayoutObject::kind`].
    pub object_type: u32,
    /// `flags`; anchors are bits 28-31 (layout-model.md 5.7).
    pub flags: u64,
    /// `bounds` in points, as stored: a child is relative to its parent's
    /// top-left (observed for button bars, groups, portals, tab controls and
    /// tab panels in the shipped files).
    pub bounds: Option<Rect>,
    pub style: Option<ObjectStyle>,
    /// The object's name (Position inspector, `LayoutObjectNames`,
    /// `GetLayoutObjectAttribute`): `4/5/<layout>/5/<object>/10`, empty or
    /// absent when unnamed (layout-model.md 5.5). `decode_from_tree`
    /// populates this.
    pub name: Option<String>,
    /// The object's UUID (`4/5/<layout>/5/<object>/d8`, 16 bytes), what
    /// `GetLayoutObjectAttribute ( ... ; "UUID" )` shows.
    pub uuid: Option<[u8; 16]>,
    /// Its place in the layout's tab order (`4/5/<layout>/5/<object>/07`, a
    /// big-endian number, 1 first; 0 when it is not in the tab order). New
    /// Record and Find mode put the focus in the first field of it
    /// (calc-functions.md 6.1, s33).
    pub tab_order: u32,
    /// Static label from the layout's per-object catalog. `decode_from_tree`
    /// populates this; complex calculation labels remain unevaluated.
    pub text: Option<String>,
    pub text_runs: Vec<TextRun>,
    /// The text as stored, with its merge-field control tokens, when
    /// `decode_from_tree` named the merge fields in [`Self::text`] from the
    /// object's stored literals: the tokens' occurrence and field references
    /// ([`Self::merge_field_specs`]) are what FileMaker shows, and a host
    /// names them again from its catalog ([`Self::resolve_merge_fields`]).
    pub merge_source: Option<String>,
    /// Character/paragraph formatting of the original static layout label.
    /// Separate from record values and from evaluated merge/calculation text.
    pub rich_text: Option<crate::rich_text::StyledText>,
    /// Stored number/date/time presentation. Applying it must not change
    /// the raw record value or the text kept in an editor.
    pub data_format: Option<crate::data_format::DataFormat>,
    /// Saved text height in layout points (`KFormat`'s `fontHeight` prefix),
    /// also present on plain labels without number/date/time settings.
    pub font_height: Option<u16>,
    /// How a field object fits a picture into its frame, from the same
    /// `KFormat` value (data-format.md "Graphic format"); `None` when the
    /// value does not hold it.
    pub graphic_format: Option<crate::data_format::GraphicFormat>,
    /// Literal argument for a button's Perform Script action, when decoded.
    pub script_parameter: Option<String>,
    /// A parameter expression exists but cannot be evaluated as a literal.
    pub script_parameter_unsupported: bool,
    /// A literal placeholder calculation (complex expressions are preserved
    /// by the source tree, but remain unevaluated here).
    pub placeholder: Option<String>,
    /// Literal tooltip calculation, gated by common object flag 0x4000.
    pub tooltip: Option<String>,
    /// The object's calculations (hide condition, conditional formatting,
    /// tooltip, placeholder, label, script parameter), from the layout's
    /// object attribute catalog; literal ones are also in the fields above.
    pub calcs: ObjectCalcs,
    pub tab: Option<TabObject>,
    pub tab_panel: Option<TabPanelObject>,
    pub graphic: Option<GraphicObject>,
    pub button: Option<ButtonObject>,
    pub popover_button: Option<PopoverButtonObject>,
    pub popover: Option<PopoverObject>,
    pub group: Option<GroupObject>,
    pub portal: Option<PortalObject>,
    /// `externalObj`, on web viewers, charts and plug-in objects.
    pub external: Option<ExternalObject>,
    /// `segmentedBarObj`, on button bars.
    pub segmented_bar: Option<SegmentedBarObject>,
    /// The object whose `childkey` lists this one; `None` for top-level
    /// objects. Set by [`ObjectStore::decode`].
    pub parent: Option<u64>,
    /// `childkey`: contained objects, in attach order.
    pub children: Vec<u64>,
    /// `fieldObj`, on field objects.
    pub field_obj: Option<FieldObject>,
    pub rotation: u32,
    /// `labelKey`: an associated label object (inferred).
    pub label_key: u64,
    /// `version`: per-type object version (forward compatibility).
    pub version: u32,
    /// Unsupported type-specific messages, unknown field numbers and fields
    /// with an unexpected wire type, preserved in wire order.
    pub other: Vec<RawField>,
    /// The object's script triggers (`4/5/<layout>/5/<object>/30`).
    pub triggers: Vec<crate::ScriptTrigger>,
}

impl LayoutObject {
    /// Decode one object; `parent` is left `None` (it comes from the store).
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut obj = LayoutObject::default();
        let mut bounds = Merged::default();
        let mut field_obj = Merged::default();
        let mut style = Merged::default();
        let mut tab = Merged::default();
        let mut tab_panel = Merged::default();
        let mut graphic = Merged::default();
        let mut button = Merged::default();
        let mut popover_button = Merged::default();
        let mut popover = Merged::default();
        let mut group = Merged::default();
        let mut portal = Merged::default();
        let mut external = Merged::default();
        let mut segmented_bar = Merged::default();
        for field in Fields::new(buf, "LayoutObject") {
            let field = field?;
            match (field.number, field.value) {
                (1, Value::Varint(v)) => obj.object_type = v as u32,
                (2, Value::Varint(v)) => obj.id = v,
                (3, Value::Varint(v)) => obj.flags = v,
                (4, Value::Bytes(b)) => bounds.push(b),
                (5, Value::Bytes(b)) => style.push(b),
                (6, value) if push_varints(&mut obj.children, value, "LayoutObject")? => {}
                (8, Value::Bytes(b)) => field_obj.push(b),
                (10, Value::Bytes(b)) => graphic.push(b),
                (11, Value::Bytes(b)) => external.push(b),
                (12, Value::Bytes(b)) => group.push(b),
                (13, Value::Bytes(b)) => tab.push(b),
                (14, Value::Bytes(b)) => tab_panel.push(b),
                (15, Value::Bytes(b)) => portal.push(b),
                (16, Value::Bytes(b)) => button.push(b),
                (17, Value::Bytes(b)) => popover_button.push(b),
                (18, Value::Bytes(b)) => popover.push(b),
                (21, Value::Bytes(b)) => segmented_bar.push(b),
                (50, Value::Varint(v)) => obj.rotation = v as u32,
                (51, Value::Varint(v)) => obj.label_key = v,
                (52, Value::Varint(v)) => obj.version = v as u32,
                _ => obj.other.push(field.into()),
            }
        }
        obj.bounds = bounds.decode(Rect::decode)?;
        obj.field_obj = field_obj.decode(FieldObject::decode)?;
        obj.style = style.decode(ObjectStyle::decode)?;
        obj.tab = tab.decode(TabObject::decode)?;
        obj.tab_panel = tab_panel.decode(TabPanelObject::decode)?;
        obj.graphic = graphic.decode(GraphicObject::decode)?;
        obj.button = button.decode(ButtonObject::decode)?;
        obj.popover_button = popover_button.decode(PopoverButtonObject::decode)?;
        obj.popover = popover.decode(PopoverObject::decode)?;
        obj.group = group.decode(GroupObject::decode)?;
        obj.portal = portal.decode(PortalObject::decode)?;
        obj.external = external.decode(ExternalObject::decode)?;
        obj.segmented_bar = segmented_bar.decode(SegmentedBarObject::decode)?;
        Ok(obj)
    }

    pub fn kind(&self) -> ObjectKind {
        ObjectKind::from_type(self.object_type)
    }

    /// The object's Slide up option, with its modifiers; `None` without it.
    /// The modifiers mean nothing on their own: FMEngine's slide record
    /// reads them only with [`sliding::UP`] (layout-model.md 5.8).
    pub fn slide_up(&self) -> Option<SlideUp> {
        (self.flags & sliding::UP != 0).then_some(SlideUp {
            directly_above: self.flags & sliding::DIRECTLY_ABOVE != 0,
            resize_part: self.flags & sliding::RESIZE_PART != 0,
        })
    }
}

/// The Inspector's Sliding & Visibility options, as bits of
/// `LayoutObject.flags` (layout-model.md 5.8): FileMaker Pro 26.0.3's
/// `PropSlidingFormatter::ApplyFormatImpl` sets each one from its property.
pub mod sliding {
    /// Sliding up (`PropBase::kSlideUp`).
    pub const UP: u64 = 0x10;
    /// Sliding left (`kSlideLeft`).
    pub const LEFT: u64 = 0x20;
    /// Also resize enclosing part (`kSlideShrinkEnclosing`).
    pub const RESIZE_PART: u64 = 0x40;
    /// Hide when printing (`kNonPrinting`).
    pub const NON_PRINTING: u64 = 0x200;
    /// Sliding up based on Only objects directly above; clear for All objects
    /// above (`kSlideUpCheckAllAbove` clears it when checked).
    pub const DIRECTLY_ABOVE: u64 = 0x8000;
}

/// An object's Slide up option ([`LayoutObject::slide_up`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SlideUp {
    /// Slide based on only the objects directly above (horizontally
    /// overlapping), rather than all objects above.
    pub directly_above: bool,
    /// Also resize the enclosing part.
    pub resize_part: bool,
}

impl LayoutObject {
    /// A web viewer's setup options: an external object of type `WEBV`
    /// (or, without its `externalObj`, of the web viewer style 86).
    pub fn web_viewer(&self) -> Option<WebViewerOptions> {
        if self.kind() != ObjectKind::External {
            return None;
        }
        match &self.external {
            Some(external) if external.type_code == ExternalObject::WEB_VIEWER => {
                Some(WebViewerOptions(external.flags))
            }
            Some(_) => None,
            None => self
                .style
                .as_ref()
                .filter(|style| style.object_type_id == 86)
                .map(|_| WebViewerOptions::default()),
        }
    }

    /// Whether a button's stored label shows. Button Setup's "Image only"
    /// (display type 1, layout-model.md 5.6.1) shows its icon alone, centered,
    /// whatever label the object stores; every other button and object shows
    /// its text.
    pub fn shows_label(&self) -> bool {
        let icon = match (&self.button, &self.popover_button) {
            (Some(button), _) => (button.library_key, button.display_type),
            (None, Some(button)) => (button.library_key, button.display_type),
            (None, None) => return true,
        };
        !matches!(icon, (key, 1) if key != 0)
    }

    /// The bound field, for field objects.
    pub fn field_spec(&self) -> Option<&FieldSpec> {
        self.field_obj.as_ref()?.field_spec.as_ref()
    }

    /// The text holding the merge-field control tokens: the stored text
    /// when decoding already named them ([`Self::merge_source`]).
    fn merge_text(&self) -> Option<&str> {
        self.merge_source.as_deref().or(self.text.as_deref())
    }

    /// Resolve already identified merge references using exact schema names.
    pub fn resolve_merge_fields(&mut self, names: &[String]) {
        let Some(text) = self.merge_text().map(str::to_owned) else {
            return;
        };
        self.text_runs = crate::split_merge_text(&text, names);
        self.text = Some(
            self.text_runs
                .iter()
                .map(|run| match run {
                    TextRun::Literal(text) => text.clone(),
                    TextRun::MergeField(name) => format!("<<{name}>>"),
                })
                .collect(),
        );
    }

    /// Field references embedded in control-token merge text. These carry raw
    /// catalog keys rather than the serial values of a field object's binding;
    /// callers use their keys directly, without [`FieldSpec::field_id`]. The
    /// token stores a BE u16 table-key length, table key, byte field-key length
    /// and field key.
    /// Unknown text remains unresolved rather than becoming a guessed field.
    pub fn merge_field_specs(&self) -> Vec<FieldSpec> {
        let Some(text) = self.merge_text() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for token in text
            .split("<<")
            .skip(1)
            .filter_map(|s| s.split_once(">>").map(|p| p.0))
        {
            if let Some(spec) = merge_field_spec(token) {
                out.push(spec);
            }
        }
        out
    }

    pub fn script_step(&self) -> Option<&ScriptStep> {
        self.button
            .as_ref()
            .and_then(|button| button.script_step.as_ref())
            .or_else(|| {
                self.group
                    .as_ref()
                    .and_then(|group| group.script_step.as_ref())
            })
            .filter(|step| step.id != 0)
    }
}

/// A merge field is `01`, an observed kind (1 or 2), the hex character
/// count, and the field-reference payload. The count grows when either
/// catalog ID needs more bytes; it is not a fixed `0e` opcode. Both
/// observed kinds carry the same raw table/field key structure. Runtime
/// references use the first repetition; no repetition is decoded here.
pub(crate) fn merge_field_spec(token: &str) -> Option<FieldSpec> {
    let mut chars = token.chars();
    if chars.next()? != '\u{1}' || !matches!(chars.next()?, '\u{1}' | '\u{2}') {
        return None;
    }
    let declared = chars.next()? as usize;
    let hex = chars.as_str();
    if hex.len() != declared || hex.len() % 2 != 0 || !hex.is_ascii() {
        return None;
    }
    let bytes: Vec<u8> = (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).ok())
        .collect::<Option<_>>()?;
    let length = usize::from(u16::from_be_bytes(bytes.get(..2)?.try_into().ok()?));
    if !(1..=8).contains(&length) {
        return None;
    }
    let table_key = bytes.get(2..2 + length).and_then(om_hbam::slot::be_uint)?;
    let field_length = usize::from(*bytes.get(2 + length)?);
    if !(1..=8).contains(&field_length) || bytes.len() != 3 + length + field_length {
        return None;
    }
    let field_key = bytes.get(3 + length..).and_then(om_hbam::slot::be_uint)?;
    Some(FieldSpec {
        table_key,
        field_key,
        repetition: 1,
        other: Vec::new(),
    })
}

/// `FMLayoutPB.LayoutObjectStore`: every object of one layout.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ObjectStore {
    /// `rootChildKey`: top-level object ids, in attach order.
    pub root: Vec<u64>,
    /// `objectDef`, in stored order.
    pub objects: Vec<LayoutObject>,
    /// Fields not decoded into the members above, verbatim in wire order.
    pub other: Vec<RawField>,
}

impl ObjectStore {
    /// Decode the store and link each object to its container.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut store = ObjectStore::default();
        for field in Fields::new(buf, "LayoutObjectStore") {
            let field = field?;
            match (field.number, field.value) {
                (1, value) if push_varints(&mut store.root, value, "LayoutObjectStore")? => {}
                (2, Value::Bytes(b)) => store.objects.push(LayoutObject::decode(b)?),
                _ => store.other.push(field.into()),
            }
        }
        store.link_parents();
        Ok(store)
    }

    /// Set `parent` from the containers' `childkey` lists. A child listed by
    /// more than one container keeps the first.
    fn link_parents(&mut self) {
        let index: HashMap<u64, usize> = self
            .objects
            .iter()
            .enumerate()
            .rev()
            .map(|(i, o)| (o.id, i))
            .collect();
        for container in 0..self.objects.len() {
            let parent = self.objects[container].id;
            for c in 0..self.objects[container].children.len() {
                let child_id = self.objects[container].children[c];
                if let Some(&child) = index.get(&child_id) {
                    let child = &mut self.objects[child];
                    if child.parent.is_none() && child.id != parent {
                        child.parent = Some(parent);
                    }
                }
            }
        }
    }

    /// The object with this id (the first one, should ids repeat).
    pub fn get(&self, id: u64) -> Option<&LayoutObject> {
        self.objects.iter().find(|o| o.id == id)
    }

    pub fn is_empty(&self) -> bool {
        self.objects.is_empty()
    }

    pub fn len(&self) -> usize {
        self.objects.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::RawValue;

    #[test]
    fn field_entry_reads_each_modes_bit_pair() {
        let entry = |flags| {
            let field = FieldObject {
                flags,
                ..Default::default()
            };
            (field.field_entry(false), field.field_entry(true))
        };
        // The shipped templates' enterable edit boxes set neither pair.
        for flags in [0xe0, 0x1200e0, 0x120020, 0x200e0] {
            assert_eq!(entry(flags), (FieldEntry::Edit, FieldEntry::Edit));
        }
        // SetFieldEntryMode's Browse table: 0x100_0000, 0x4, 0x100_0004.
        assert_eq!(entry(0xe0 | 0x100_0000).0, FieldEntry::SelectOnly);
        assert_eq!(entry(0xe0 | 0x4), (FieldEntry::ViewOnly, FieldEntry::Edit));
        assert_eq!(entry(0x100_0004).0, FieldEntry::Calculated);
        // Its Find table: 0x200_0000, 0x10, 0x200_0010.
        assert_eq!(
            entry(0x200_0000),
            (FieldEntry::Edit, FieldEntry::SelectOnly)
        );
        assert_eq!(entry(0x10), (FieldEntry::Edit, FieldEntry::ViewOnly));
        assert_eq!(entry(0x200_0010).1, FieldEntry::Calculated);
    }

    /// Minimal encoder for building test messages.
    fn varint(mut v: u64, out: &mut Vec<u8>) {
        while v >= 0x80 {
            out.push(v as u8 | 0x80);
            v >>= 7;
        }
        out.push(v as u8);
    }

    fn tag(number: u32, wire: u8, out: &mut Vec<u8>) {
        varint(u64::from(number) << 3 | u64::from(wire), out);
    }

    fn uint(number: u32, v: u64, out: &mut Vec<u8>) {
        tag(number, 0, out);
        varint(v, out);
    }

    fn double(number: u32, v: f64, out: &mut Vec<u8>) {
        tag(number, 1, out);
        out.extend_from_slice(&v.to_le_bytes());
    }

    fn bytes(number: u32, b: &[u8], out: &mut Vec<u8>) {
        tag(number, 2, out);
        varint(b.len() as u64, out);
        out.extend_from_slice(b);
    }

    fn rect(top: f64, left: f64, bottom: f64, right: f64) -> Vec<u8> {
        let mut out = Vec::new();
        double(1, top, &mut out);
        double(2, left, &mut out);
        double(3, bottom, &mut out);
        double(4, right, &mut out);
        out
    }

    fn field_object(id: u64, table: u64, field: u64) -> Vec<u8> {
        let mut spec = Vec::new();
        uint(1, table, &mut spec);
        uint(2, field, &mut spec);
        let mut field_obj = Vec::new();
        bytes(2, &spec, &mut field_obj);
        uint(5, 6, &mut field_obj);
        let mut obj = Vec::new();
        uint(1, 1, &mut obj);
        uint(2, id, &mut obj);
        uint(3, 0x3000_0000, &mut obj);
        bytes(4, &rect(10.0, 20.0, 30.0, 120.0), &mut obj);
        bytes(8, &field_obj, &mut obj);
        obj
    }

    #[test]
    fn control_token_merge_references_preserve_table_and_field_identity() {
        let object = LayoutObject {
            text: Some("<<\u{1}\u{1}\u{e}0003D000020108>>".into()),
            ..Default::default()
        };
        let fields = object.merge_field_specs();
        assert_eq!(fields.len(), 1);
        assert_eq!((fields[0].table_key, fields[0].field_key), (0xd00002, 8));
        let malformed = LayoutObject {
            text: Some("<<\u{1}\u{1}\u{e}0003D0000201>>".into()),
            ..Default::default()
        };
        assert!(malformed.merge_field_specs().is_empty());
    }

    #[test]
    fn merge_token_length_and_kind_support_raw_multibyte_field_ids() {
        for (kind, table) in [(1, 0xd00001), (2, 0xd00016)] {
            let token = format!("<<\u{1}{}\u{10}0003{table:06X}028123>>", char::from(kind));
            let object = LayoutObject {
                text: Some(token),
                ..Default::default()
            };
            let fields = object.merge_field_specs();
            assert_eq!(fields.len(), 1);
            assert_eq!((fields[0].table_key, fields[0].field_key), (table, 0x8123));
            assert_eq!(fields[0].repetition, 1, "the kind byte is not a repetition");
        }
        for token in [
            "<<\u{1}\u{1}\u{e}0003D00001028123>>",  // wrong declared length
            "<<\u{1}\u{3}\u{10}0003D00001028123>>", // unsupported kind
            "<<\u{1}\u{1}\u{10}0003D00001028G23>>", // non-hex payload
            "<<\u{1}\u{1}\u{f}0003D0000102812>>",   // truncated payload
        ] {
            let object = LayoutObject {
                text: Some(token.into()),
                ..Default::default()
            };
            assert!(object.merge_field_specs().is_empty(), "{token:?}");
        }
    }

    #[test]
    fn field_specs_convert_serial_field_keys_to_catalog_ids() {
        for (serial, catalog) in [
            (127, 127),
            (128, 0x8000),
            (255, 0x807f),
            (256, 0x8080),
            (300, 0x80ac),
            (512, 0x8180),
            (0x4080, 0xc00000),
        ] {
            let object = LayoutObject::decode(&field_object(1, 1065089, serial)).unwrap();
            let spec = object.field_spec().unwrap();
            assert_eq!(spec.field_key, serial);
            assert_eq!(spec.field_id(), Some(catalog));
        }
        assert_eq!(
            FieldSpec {
                field_key: u64::MAX,
                ..Default::default()
            }
            .field_id(),
            None
        );
    }

    #[test]
    fn field_object_decodes() {
        let obj = LayoutObject::decode(&field_object(7, 1065090, 2)).unwrap();
        assert_eq!(
            (obj.id, obj.kind(), obj.flags),
            (7, ObjectKind::Field, 0x3000_0000)
        );
        let bounds = obj.bounds.as_ref().unwrap();
        assert_eq!(
            (bounds.top, bounds.left, bounds.bottom, bounds.right),
            (10.0, 20.0, 30.0, 120.0)
        );
        assert_eq!((bounds.width(), bounds.height()), (100.0, 20.0));
        let spec = obj.field_spec().unwrap();
        assert_eq!((spec.table_key, spec.field_key), (1065090, 2));
        assert_eq!(obj.field_obj.as_ref().unwrap().display_type, 6);
        assert!(obj.other.is_empty());
    }

    #[test]
    fn web_viewer_options_decode_from_the_external_object() {
        // The Contacts template's "website" web viewer: type 13, externalObj
        // { type 'WEBV', subType 0, flags 0x8003 } as stored.
        let mut buf = Vec::new();
        uint(1, 13, &mut buf);
        uint(2, 7, &mut buf);
        bytes(
            11,
            &[
                8, 0xd6, 0x84, 0x95, 0xba, 0x05, 0x10, 0, 0x18, 0x83, 0x80, 0x02,
            ],
            &mut buf,
        );
        let obj = LayoutObject::decode(&buf).unwrap();
        let external = obj.external.as_ref().unwrap();
        assert_eq!(
            (external.type_code, external.sub_type, external.flags),
            (ExternalObject::WEB_VIEWER, 0, 0x8003)
        );
        assert!(obj.other.is_empty());
        let options = obj.web_viewer().unwrap();
        assert!(options.allows_interaction() && options.shows_in_find_mode());
        assert!(!options.shows_progress_bar() && !options.shows_status_messages());
        assert!(options.encodes_url() && !options.allows_javascript_scripts());

        // Bit 0x20 turns URL encoding off; 0x40 allows FileMaker scripts.
        let options = WebViewerOptions(0x8061);
        assert!(!options.encodes_url() && options.allows_javascript_scripts());
        // A chart is an external object too, but not a web viewer.
        let mut chart = Vec::new();
        uint(1, 13, &mut chart);
        let mut external = Vec::new();
        uint(1, u64::from(u32::from_be_bytes(*b"CHRT")), &mut external);
        bytes(11, &external, &mut chart);
        assert_eq!(LayoutObject::decode(&chart).unwrap().web_viewer(), None);
    }

    #[test]
    fn unknown_and_undecoded_fields_are_kept() {
        let mut buf = field_object(1, 2, 3);
        bytes(5, &[0x18, 0x3d], &mut buf); // style {objectTypeId: 61}
        uint(99, 42, &mut buf); // not in the schema
        double(2, 1.5, &mut buf); // key with the wrong wire type
        let obj = LayoutObject::decode(&buf).unwrap();
        assert_eq!(obj.id, 1);
        assert_eq!(obj.style.as_ref().unwrap().object_type_id, 61);
        let other: Vec<_> = obj.other.iter().map(|f| (f.number, &f.value)).collect();
        assert_eq!(
            other,
            [
                (99, &RawValue::Varint(42)),
                (2, &RawValue::Fixed64(1.5f64.to_bits())),
            ]
        );
    }

    #[test]
    fn repeated_message_occurrences_merge() {
        let mut buf = Vec::new();
        let mut first = Vec::new();
        double(1, 5.0, &mut first);
        double(3, 9.0, &mut first);
        bytes(4, &first, &mut buf);
        let mut second = Vec::new();
        double(3, 50.0, &mut second);
        bytes(4, &second, &mut buf);
        let bounds = LayoutObject::decode(&buf).unwrap().bounds.unwrap();
        assert_eq!((bounds.top, bounds.bottom), (5.0, 50.0));
    }

    #[test]
    fn store_links_parents_from_packed_and_unpacked_child_keys() {
        let mut portal = Vec::new();
        uint(1, 9, &mut portal);
        uint(2, 1, &mut portal);
        uint(6, 2, &mut portal);
        bytes(6, &[3, 4], &mut portal);
        let mut store = Vec::new();
        uint(1, 1, &mut store);
        bytes(2, &portal, &mut store);
        for id in 2..=4 {
            bytes(2, &field_object(id, 1, id), &mut store);
        }
        let mut lone = Vec::new();
        uint(2, 5, &mut lone);
        bytes(2, &lone, &mut store);

        let store = ObjectStore::decode(&store).unwrap();
        assert_eq!(store.root, [1]);
        assert_eq!(store.len(), 5);
        let portal = store.get(1).unwrap();
        assert_eq!(
            (portal.kind(), &portal.children[..]),
            (ObjectKind::Portal, &[2, 3, 4][..])
        );
        let parents: Vec<_> = store.objects.iter().map(|o| o.parent).collect();
        assert_eq!(parents, [None, Some(1), Some(1), Some(1), None]);
        assert_eq!(store.get(5).unwrap().kind(), ObjectKind::Unknown(0));
    }

    #[test]
    fn button_references_ignore_the_filler_after_the_key() {
        let step = |reserved: &[u8]| ScriptStep {
            reserved: reserved.to_vec(),
            ..Default::default()
        };
        // Template filler, the zero filler most buttons carry, and a
        // two-byte segment.
        assert_eq!(step(&[2, 1, 3, 0xbe, 0, 0]).local_reference(), Some(3));
        assert_eq!(step(&[2, 1, 3, 0, 0x6f, 0]).local_reference(), Some(3));
        assert_eq!(
            step(&[3, 2, 0x80, 0xc8, 0, 0]).local_reference(),
            Some(0x80c8)
        );
        assert_eq!(step(&[2, 3, 1, 2]).local_reference(), None);
        assert_eq!(step(&[0, 0, 0]).local_reference(), None);
    }

    #[test]
    fn button_bars_keep_their_active_segment_key() {
        // The Tasks template's navigation bar (en.fmp12): segments 2 and 3,
        // `segmentedBarObj { flags 0, activeSegmentKey 2 }`; its other
        // bars store no key (None).
        let mut buf = Vec::new();
        uint(1, 18, &mut buf);
        uint(2, 1, &mut buf);
        uint(6, 2, &mut buf);
        uint(6, 3, &mut buf);
        bytes(21, &[0x08, 0x00, 0x10, 0x02], &mut buf);
        let bar = LayoutObject::decode(&buf).unwrap();
        assert_eq!(bar.kind(), ObjectKind::ButtonBar);
        assert_eq!(bar.segmented_bar.as_ref().unwrap().active_segment_key, 2);
        assert!(bar.other.is_empty());

        let mut none = Vec::new();
        uint(1, 18, &mut none);
        bytes(21, &[0x08, 0x00], &mut none);
        let bar = LayoutObject::decode(&none).unwrap();
        assert_eq!(bar.segmented_bar.unwrap().active_segment_key, 0);
    }

    #[test]
    fn empty_store() {
        let store = ObjectStore::decode(&[]).unwrap();
        assert!(store.is_empty() && store.root.is_empty());
    }

    #[test]
    fn malformed_object_reports_its_message() {
        let mut buf = Vec::new();
        bytes(2, &[0x22, 0x02, 0x09, 0x00], &mut buf); // bounds with a truncated double
        let err = ObjectStore::decode(&buf).unwrap_err();
        assert_eq!((err.message, err.reason), ("Rect", "truncated value"));
    }
}
