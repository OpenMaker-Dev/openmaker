//! FileMaker layout decoding: the `FMLayoutPB` protobuf records that
//! [`om_hbam`] returns per layout, as typed structs.
//!
//! Spec: `docs/re/protobuf/{FMLayout,LayoutObject}.proto` (schemas) and
//! `docs/re/layout-model.md` (semantics). The input is one
//! [`om_hbam::Layout`] (from a file or a wire buffer):
//!
//! ```text
//! Layout.layout_def   -> LayoutDef   (width, table occurrence, view flags)
//! Layout.parts[].def  -> Part        (kind, top, height)
//! Layout.object_store -> ObjectStore (objects, bounds, tree, field binding)
//! ```
//!
//! Decoding uses a small hand-written proto2 wire reader ([`wire`]). Scalars
//! read as their proto2 default (zero) when absent, as the C++ getters
//! FMEngine uses return them; embedded messages are `Option`.
//!
//! Rich decoding includes object styles, the embedded theme cascade, container
//! coordinates and static labels. Use [`Layout::decode_from_tree`] when the
//! source tree is available: the flat catalog omits labels and theme records.
//! Unknown protobuf fields remain owned in each message's `other` collection.

pub mod calcs;
pub mod data_format;
pub mod encode;
pub mod layout;
pub mod library;
pub mod object;
pub mod pagination;
pub mod panels;
pub mod paper;
pub mod rich_text;
pub mod style;
pub mod triggers;
pub mod wire;

pub use calcs::{CfCondition, ConditionalFormat, ObjectCalcs};
pub use data_format::{
    DataFormat, DateFormat, GraphicAlign, GraphicFormat, NumberFormat, TimeFormat,
};
pub use layout::{occurrence_id, stored_layout_word, BreakField, LayoutDef, Part, PartKind};
pub use library::{LibraryImage, LibraryImageFormat};
pub use object::{
    sliding, ButtonObject, ExternalObject, FieldEntry, FieldObject, FieldSpec, GraphicObject,
    GroupObject, LayoutObject, ObjectKind, ObjectStore, PopoverButtonObject, PopoverObject,
    PortalObject, Rect, ScriptStep, SegmentedBarObject, SlideUp, TabObject, TabPanelObject,
    TextRun, WebViewerOptions,
};
pub use rich_text::{CharacterStyle, ParagraphStyle, StyledRun, StyledText};
pub use style::{
    Color, Coord, Length, ObjectStyle, Property, ResolvedStyle, StyleSet, ThemeStyles,
};
pub use triggers::ScriptTrigger;
pub use wire::{Error, RawField, RawValue, Result};

/// `KScriptTriggers`: the trigger record of a layout or object.
const TRIGGERS: u64 = 0x30;
/// A catalog member's name; under a layout's object attributes, the
/// object's name (layout-model.md 5.5).
const NAME: u64 = 0x10;
/// A catalog member's UUID; under a layout's object attributes, the
/// object's.
const UUID: u64 = 0xd8;
/// An object's place in the layout's tab order (`KTabOrder`, 1 first; 0
/// or absent: not in it).
const TAB_ORDER: u64 = 0x07;
/// The layout's separate table-view layout `4/5/<id>/0d` (file-metadata.md
/// 7.1), with an object list of its own.
const TABLE_VIEW: u64 = 0x0d;

/// One decoded layout.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    /// Layout id (`4/5/<id>`).
    pub id: u64,
    pub name: String,
    /// `None` when the layout has no `LayoutDef` record.
    pub def: Option<LayoutDef>,
    /// Parts in catalog key order.
    pub parts: Vec<Part>,
    /// Empty when the layout has no objects (or no object store record).
    pub objects: ObjectStore,
    /// Referenced file-contained theme (`6/5/<key>`).
    pub theme_key: Option<u64>,
    pub theme_styles: ThemeStyles,
    /// File font-list ids (`25/5/<id>/16`). Compiled theme property 64
    /// may reference these by integer rather than carrying a string.
    pub font_families: std::collections::BTreeMap<u64, String>,
    /// Referenced embedded images from file library `31/5/<key>/<QuadChar>`.
    /// SVG originals take priority over raster previews.
    pub library_images: std::collections::BTreeMap<u64, LibraryImage>,
    /// The layout's script triggers (`4/5/<id>/30`).
    pub triggers: Vec<ScriptTrigger>,
    /// The triggers of the table view's objects by object id
    /// (`4/5/<id>/0d/5/<object>/30`; FileMaker's XML does not export them).
    pub table_view_triggers: std::collections::BTreeMap<u64, Vec<ScriptTrigger>>,
}

impl Layout {
    pub fn decode(layout: &om_hbam::Layout) -> Result<Self> {
        Ok(Layout {
            theme_key: None,
            theme_styles: ThemeStyles::default(),
            font_families: Default::default(),
            library_images: Default::default(),
            triggers: Vec::new(),
            table_view_triggers: Default::default(),
            id: layout.id,
            name: layout.name.clone(),
            def: layout
                .layout_def
                .as_deref()
                .map(LayoutDef::decode)
                .transpose()?,
            parts: layout
                .parts
                .iter()
                .map(|p| Part::decode(p.id, &p.part_def))
                .collect::<Result<_>>()?,
            objects: match &layout.object_store {
                Some(store) => ObjectStore::decode(store)?,
                None => ObjectStore::default(),
            },
        })
    }

    /// Decode one layout with its per-object text and its referenced theme.
    /// The tree may be a full file or merged hosted layout (4), theme (6),
    /// font (25) and image-library (31) catalogs.
    pub fn decode_from_tree(tree: &om_hbam::KeyTree<'_>, id: u64) -> Result<Option<Self>> {
        let Some(node) = tree.at(&[4, 5, id]) else {
            return Ok(None);
        };
        let bytes = |n: &om_hbam::Node<'_>, key| {
            n.get(key)
                .and_then(|n| n.assembled())
                .map(|b| b.into_owned())
        };
        let raw = om_hbam::Layout {
            id,
            name: node
                .get(16)
                .and_then(|n| n.text())
                .and_then(std::result::Result::ok)
                .unwrap_or_default(),
            layout_def: bytes(node, 2),
            object_store: bytes(node, 7),
            parts: node
                .get(3)
                .into_iter()
                .flat_map(|n| n.children.iter())
                .filter_map(|n| {
                    Some(om_hbam::LayoutPart {
                        id: n.id()?,
                        part_def: bytes(n, 2)?,
                    })
                })
                .collect(),
        };
        let mut layout = Self::decode(&raw)?;
        layout.library_images = library::decode_images(tree, &layout.objects);
        // Modern files keep the effective view flags in the layout's catalog
        // member key 0xf1, not in `LayoutDef.flags` (layout-model.md 4.1,
        // 4.2); the protobuf's usual zero would lose the default and allowed
        // views. FileMaker keeps a copy in the catalog index's entry
        // `[4, 1, 7, L]`, read when the member has none, by the same rule as
        // the host's Layout Setup (`stored_layout_word`): a dialog started
        // from 0 would drop what only the entry holds (#977).
        // Each copy's own slot value, as the host's planner reads it: a split
        // value is not a word either reads.
        let member = node.get(0xf1).and_then(|n| n.value);
        let entry = tree.at(&[4, 1, 7, id, 0xf1]).and_then(|n| n.value);
        if let Some(flags) = stored_layout_word(member, entry) {
            if let Some(def) = &mut layout.def {
                def.flags = flags;
            }
        }

        layout.font_families = tree
            .at(&[25, 5])
            .into_iter()
            .flat_map(|n| n.children.iter())
            .filter_map(|n| Some((n.id()?, n.get(16)?.text()?.ok()?)))
            .collect();
        if let Some(reference) = bytes(node, 11) {
            let key = catalog_reference(&reference);
            layout.theme_key = key;
            if let Some(theme) = key
                .and_then(|k| tree.at(&[6, 5, k, 22]))
                .and_then(|n| n.assembled())
            {
                layout.theme_styles = ThemeStyles::decode(&theme)?
            }
        }
        layout.triggers = node.get(TRIGGERS).map(triggers::decode).unwrap_or_default();
        layout.table_view_triggers = node
            .at(&[TABLE_VIEW, 5])
            .into_iter()
            .flat_map(|objects| objects.children.iter())
            .filter_map(|object| {
                let triggers = triggers::decode(object.get(TRIGGERS)?);
                let id = om_hbam::serial::decode(object.segment.0)?;
                Some((id, triggers)).filter(|(_, t)| !t.is_empty())
            })
            .collect();
        if let Some(attrs) = node.get(5) {
            for object in &mut layout.objects.objects {
                // Keyed by the object's id as a serial key (`80 09` is object
                // 137, file-metadata.md 1), like every catalog member.
                if let Some(attr) = attrs.child(&om_hbam::serial::encode(object.id)) {
                    object.name = attr
                        .get(NAME)
                        .and_then(|n| n.text())
                        .and_then(std::result::Result::ok)
                        .filter(|name| !name.is_empty());
                    object.uuid = attr
                        .get(UUID)
                        .and_then(|n| n.assembled())
                        .and_then(|b| <[u8; 16]>::try_from(b.as_ref()).ok());
                    object.tab_order = attr
                        .get(TAB_ORDER)
                        .and_then(|n| n.uint_value())
                        .map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX));
                    object.triggers = attr.get(TRIGGERS).map(triggers::decode).unwrap_or_default();
                    if let Some(data) = attr.get(17) {
                        for step in object
                            .button
                            .iter_mut()
                            .filter_map(|button| button.script_step.as_mut())
                            .chain(
                                object
                                    .group
                                    .iter_mut()
                                    .filter_map(|group| group.script_step.as_mut()),
                            )
                        {
                            step.data = Some(data.to_owned_node());
                        }
                    }
                    let format = attr.get(13).and_then(|node| node.assembled());
                    object.font_height = format.as_deref().and_then(DataFormat::font_height);
                    object.data_format = format.as_deref().and_then(DataFormat::decode);
                    object.graphic_format = format.as_deref().and_then(GraphicFormat::decode);
                    let rich_text = attr.get(1).and_then(StyledText::decode).map(|mut text| {
                        text.resolve_fonts(&layout.font_families);
                        text
                    });
                    object.calcs = calcs::decode(attr, object.flags, rich_text.is_some());
                    object.text = rich_text.as_ref().map(|t| t.text.clone()).or_else(|| {
                        attr.at(&[2, 6])
                            .or_else(|| attr.at(&[2, 5]))
                            .and_then(|n| n.assembled())
                            .and_then(|b| literal_text(&b))
                    });
                    object.rich_text = rich_text.filter(|text| {
                        !text.character_runs.is_empty() || !text.paragraph_runs.is_empty()
                    });
                    object.placeholder = attr
                        .at(&[6, 6])
                        .or_else(|| attr.at(&[6, 5]))
                        .and_then(|n| n.assembled())
                        .and_then(|b| literal_text(&b));
                    object.tooltip = (object.flags & 0x4000 != 0)
                        .then(|| {
                            attr.at(&[3, 6])
                                .or_else(|| attr.at(&[3, 5]))
                                .and_then(|n| n.assembled())
                                .and_then(|b| literal_text(&b))
                        })
                        .flatten();
                    let fields: Vec<String> = attr
                        .get(17)
                        .into_iter()
                        .flat_map(|n| n.children.iter())
                        .filter_map(|n| {
                            n.at(&[5, 6])
                                .or_else(|| n.at(&[5, 5]))
                                .and_then(|n| n.assembled())
                                .and_then(|b| literal_text(&b))
                        })
                        .collect();
                    if object.script_step().is_some_and(|step| step.id == 1) {
                        let parameter = attr
                            .get(17)
                            .and_then(|node| node.children.iter().find(|n| n.id() == Some(0)))
                            .and_then(|node| node.at(&[5, 6]).or_else(|| node.at(&[5, 5])))
                            .and_then(|node| node.assembled());
                        object.script_parameter = parameter.as_deref().and_then(literal_text);
                        object.script_parameter_unsupported =
                            parameter.is_some() && object.script_parameter.is_none();
                    }
                    if let Some(text) = &object.text {
                        object.text_runs = split_merge_text(text, &fields);
                    }
                    if object
                        .text_runs
                        .iter()
                        .any(|run| matches!(run, TextRun::MergeField(_)))
                    {
                        object.merge_source = object.text.clone();
                    }
                    if !object.text_runs.is_empty() {
                        object.text = Some(
                            object
                                .text_runs
                                .iter()
                                .map(|r| match r {
                                    TextRun::Literal(s) => s.clone(),
                                    TextRun::MergeField(name) => format!("<<{name}>>"),
                                })
                                .collect(),
                        );
                    }
                }
            }
        }
        Ok(Some(layout))
    }

    /// Bounds in layout coordinates, adding every ancestor's origin. Invalid
    /// cycles and missing ancestor geometry return `None`.
    pub fn absolute_bounds(&self, id: u64) -> Option<Rect> {
        let object = self.objects.get(id)?;
        let mut bounds = object.bounds.clone()?;
        let mut parent = object.parent;
        let mut seen = std::collections::HashSet::from([id]);
        while let Some(id) = parent {
            if !seen.insert(id) {
                return None;
            }
            let object = self.objects.get(id)?;
            let origin = object.bounds.as_ref()?;
            bounds.top += origin.top;
            bounds.bottom += origin.top;
            bounds.left += origin.left;
            bounds.right += origin.left;
            parent = object.parent;
        }
        Some(bounds)
    }

    /// How far a portal's rows sit inside `portal`: its left and top border
    /// widths, where its style draws those borders (`border-style` neither
    /// none nor hidden). FileMaker Server's `FieldBounds` places a portal's
    /// fields there (layout-model.md 5.4), and FileMaker Pro's object index
    /// keys them there (10.5).
    pub fn portal_row_inset(&self, portal: &LayoutObject) -> (f64, f64) {
        const BORDER_NONE: i32 = 1;
        const BORDER_HIDDEN: i32 = 2;
        let style = self.object_style(portal, 1, 1);
        let drawn = |side: usize| {
            let shown =
                style.border_style[side].is_some_and(|s| s != BORDER_NONE && s != BORDER_HIDDEN);
            if shown {
                style.border_width[side].unwrap_or(0.0)
            } else {
                0.0
            }
        };
        // Sides in CSS order: top, right, bottom, left.
        (drawn(3), drawn(0))
    }

    pub fn object_style(&self, object: &LayoutObject, component: i32, state: i32) -> ResolvedStyle {
        self.resolve_style(
            object.style.as_ref(),
            object_type_fallback(object),
            component,
            state,
        )
    }

    /// One cascaded property that is not part of [`ResolvedStyle`].
    pub fn object_property<'a>(
        &'a self,
        object: &'a LayoutObject,
        component: i32,
        state: i32,
        property: i32,
    ) -> Option<&'a Property> {
        style::resolve_property(
            object.style.as_ref(),
            object_type_fallback(object),
            self.def.as_ref().and_then(|def| def.local_styles.as_ref()),
            &self.theme_styles,
            component,
            state,
            property,
        )
    }

    /// The properties a conditional format sets on `object` for
    /// `component` in `state`: only those of the condition's own style,
    /// which override the object's cascaded values while its formula is
    /// true (layout-model.md 6.7). Overlay the result on
    /// [`Layout::object_style`] with [`ResolvedStyle::overlay`].
    pub fn conditional_style(
        &self,
        object: &LayoutObject,
        condition: &CfCondition,
        component: i32,
        state: i32,
    ) -> ResolvedStyle {
        let Some(style) = condition.style.as_ref() else {
            return ResolvedStyle::default();
        };
        // Only the condition's local styles: no theme or named styles.
        let local = ObjectStyle {
            local_styles: style.local_styles.clone(),
            ..ObjectStyle::default()
        };
        style::resolve_style(
            Some(&local),
            object_type_fallback(object),
            None,
            &ThemeStyles::default(),
            component,
            state,
        )
    }

    /// `part`'s style. A part edge whose border style is unset has no border
    /// line, whatever width and color it stores: FileMaker Pro 26 strokes only
    /// the starter lists' header and sub-summary edges whose style is set (top
    /// and bottom), not their unset sides (layout-model.md 4.3).
    pub fn part_style(&self, part: &Part, component: i32, state: i32) -> ResolvedStyle {
        let mut style =
            self.resolve_style(part.style.as_ref(), part_fallback(part), component, state);
        for side in 0..4 {
            if style.border_style[side].is_none() {
                style.border_width[side] = None;
            }
        }
        style
    }

    /// One cascaded property of `part` that is not part of [`ResolvedStyle`].
    pub fn part_property<'a>(
        &'a self,
        part: &'a Part,
        component: i32,
        state: i32,
        property: i32,
    ) -> Option<&'a Property> {
        style::resolve_property(
            part.style.as_ref(),
            part_fallback(part),
            self.def.as_ref().and_then(|def| def.local_styles.as_ref()),
            &self.theme_styles,
            component,
            state,
            property,
        )
    }
    pub fn background_style(&self, component: i32, state: i32) -> ResolvedStyle {
        self.resolve_style(
            self.def.as_ref().and_then(|d| d.background_style.as_ref()),
            20,
            component,
            state,
        )
    }
    fn resolve_style(
        &self,
        object: Option<&ObjectStyle>,
        fallback: i32,
        component: i32,
        state: i32,
    ) -> ResolvedStyle {
        let local = self.def.as_ref().and_then(|d| d.local_styles.as_ref());
        let mut result = style::resolve_style(
            object,
            fallback,
            local,
            &self.theme_styles,
            component,
            state,
        );
        if result.font_family.is_none() {
            result.font_family = style::resolve_property(
                object,
                fallback,
                local,
                &self.theme_styles,
                component,
                state,
                64,
            )
            .and_then(|p| p.int)
            .and_then(|id| u64::try_from(id).ok())
            .and_then(|id| self.font_families.get(&id))
            .cloned();
        }
        result
    }

    /// Layout width in points (`LayoutDef.baseWidth`, 0 without a definition).
    pub fn width(&self) -> f64 {
        self.def.as_ref().map_or(0.0, |d| d.base_width)
    }

    /// Layout height in points: the bottom of the lowest part.
    pub fn height(&self) -> f64 {
        self.parts.iter().map(Part::bottom).fold(0.0, f64::max)
    }
}

/// The object type whose styles an object without its own type id uses
/// (layout-model.md 6.7).
/// The style object type a part's style falls back to (layout-model.md
/// 4.3, 6.2).
fn part_fallback(part: &Part) -> i32 {
    match part.kind() {
        PartKind::TitleHeader => 31,
        PartKind::Header => 32,
        PartKind::LeadingGrandSummary => 33,
        PartKind::LeadingSubSummary => 100,
        PartKind::Body => 34,
        PartKind::TrailingSubSummary => 200,
        PartKind::TrailingGrandSummary => 37,
        PartKind::Footer => 38,
        PartKind::TitleFooter => 39,
        PartKind::TopNavigation => 40,
        PartKind::BottomNavigation => 41,
        PartKind::Other(_) => 30,
    }
}

fn object_type_fallback(object: &LayoutObject) -> i32 {
    match object.kind() {
        ObjectKind::Field => match object.field_obj.as_ref().map(|f| f.display_type) {
            Some(6)
                if object
                    .field_obj
                    .as_ref()
                    .is_some_and(|field| field.has_icon()) =>
            {
                66
            }
            Some(1)
                if object
                    .field_obj
                    .as_ref()
                    .is_some_and(|field| field.has_icon()) =>
            {
                62
            }
            Some(2) => 63,
            Some(3) => 64,
            Some(4) => 65,
            _ => 61,
        },
        ObjectKind::Text => 70,
        ObjectKind::Graphic => 84,
        ObjectKind::Line => 50,
        ObjectKind::Rectangle => 51,
        ObjectKind::RoundedRectangle => 52,
        ObjectKind::Oval => 53,
        ObjectKind::Group => 87,
        ObjectKind::Portal => 83,
        ObjectKind::Button | ObjectKind::PopoverButton => 80,
        ObjectKind::TabControl => 81,
        ObjectKind::TabPanel => 82,
        ObjectKind::External => 86,
        ObjectKind::Popover => 89,
        ObjectKind::SlideControl => 91,
        ObjectKind::SlidePanel => 92,
        ObjectKind::ButtonBar => 78,
        ObjectKind::Unknown(_) => 1,
    }
}

/// Decode every layout of a catalog, in catalog order.
pub fn decode_all(catalog: &om_hbam::Catalog) -> Result<Vec<Layout>> {
    catalog.layouts.iter().map(Layout::decode).collect()
}

/// The key `T` of the theme `[6, 5, T]` that layout `id`'s member in
/// `tree` names (its key `0b`; layout-model.md 4.1), read as
/// [`Layout::decode_from_tree`] reads it, without decoding the layout. A
/// reader that holds only some themes reads this one before decoding.
pub fn theme_reference(tree: &om_hbam::KeyTree<'_>, id: u64) -> Option<u64> {
    let reference = tree.at(&[4, 5, id, 11])?.assembled()?;
    catalog_reference(&reference)
}

/// A catalog reference: an unsigned integer, preceded by its byte count
/// as a key string, or plain.
fn catalog_reference(reference: &[u8]) -> Option<u64> {
    if reference.first().copied().map(usize::from) == Some(reference.len().saturating_sub(1)) {
        om_hbam::slot::be_uint(&reference[1..])
    } else {
        om_hbam::slot::be_uint(reference)
    }
}

/// A single compiled string literal followed by the expression terminator.
/// Never scan complex expressions for strings: that would show fragments of
/// field names or URLs in place of the calculated display value.
fn literal_text(bytes: &[u8]) -> Option<String> {
    let [0x13, len, rest @ ..] = bytes else {
        return None;
    };
    let len = usize::from(*len);
    if rest.len() != len + 1 || rest[len] != 0 {
        return None;
    }
    om_scsu::decode_scsu(&rest[..len]).ok()
}

fn split_merge_text(text: &str, fields: &[String]) -> Vec<TextRun> {
    let mut out = Vec::new();
    let mut at = 0;
    let mut literal = 0;
    let mut field = fields.iter();
    while let Some(start) = text[at..].find("<<").map(|i| i + at) {
        let Some(end) = text[start + 2..].find(">>").map(|i| i + start + 2) else {
            break;
        };
        at = end + 2;
        if object::merge_field_spec(&text[start + 2..end]).is_none() {
            continue;
        }
        let Some(name) = field.next() else { break };
        if start > literal {
            out.push(TextRun::Literal(text[literal..start].to_owned()))
        }
        out.push(TextRun::MergeField(name.clone()));
        literal = end + 2;
    }
    if literal < text.len() {
        out.push(TextRun::Literal(text[literal..].to_owned()))
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The starter lists' headers store 0.25-point borders on every side but a
    /// style on the top and bottom only; FileMaker Pro 26 strokes only those
    /// two (#536), so the unset sides lose their width.
    #[test]
    fn part_edges_with_an_unset_border_style_have_no_line() {
        use crate::style::{ComponentStyle, Property, StyleSet};
        let mut properties = Vec::new();
        for side in 0..4 {
            properties.push(Property {
                id: 28 + side,
                length: Some(Length {
                    value: 0.25,
                    unit: 6,
                    ..Default::default()
                }),
                ..Default::default()
            });
        }
        for side in [0, 2] {
            properties.push(Property {
                id: 24 + side,
                int: Some(5),
                ..Default::default()
            });
        }
        let layout = Layout {
            parts: vec![Part {
                part_type: 1,
                height: 30.0,
                style: Some(ObjectStyle {
                    local_styles: Some(StyleSet {
                        components: vec![ComponentStyle {
                            component: 1,
                            state: 1,
                            properties,
                            ..Default::default()
                        }],
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let style = layout.part_style(&layout.parts[0], 1, 1);
        assert_eq!(style.border_width, [Some(0.25), None, Some(0.25), None]);
    }
    #[test]
    fn effective_view_flags_come_from_catalog_member_f1() {
        use om_hbam::{KeySegment, KeyTree};
        // Minimal synthetic LayoutDef with no flags field, as in the actual
        // Contacts file. Distinct member flags select List versus Form.
        for (member_flags, expected) in [(0x10f3_0a0b_u64, 0x200_u64), (0x10f3_080d, 0)] {
            let mut tree = KeyTree::default();
            let member = member_flags.to_be_bytes();
            let definition = [0x10, 0x82, 0x81, 0x41];
            tree.insert(
                &[
                    KeySegment(&[4]),
                    KeySegment(&[5]),
                    KeySegment(&[2]),
                    KeySegment(&[2]),
                ],
                om_hbam::slot::ChunkKind::Leaf,
                Some(&definition),
            )
            .unwrap();
            tree.insert(
                &[
                    KeySegment(&[4]),
                    KeySegment(&[5]),
                    KeySegment(&[2]),
                    KeySegment(&[0xf1]),
                ],
                om_hbam::slot::ChunkKind::Leaf,
                Some(&member),
            )
            .unwrap();
            let layout = Layout::decode_from_tree(&tree, 2).unwrap().unwrap();
            assert_eq!(layout.def.unwrap().flags & 0x200, expected);
        }
        // A member without flags takes its index entry's `[4, 1, 7, L, f1]`,
        // as the host's Layout Setup reads them: a dialog started from the
        // protobuf's 0 would clear "Use fixed page margins" (`0x40`) on a
        // rename (#977).
        let mut tree = KeyTree::default();
        let definition = [0x10, 0x82, 0x81, 0x41];
        let entry = 0x10f3_084d_u64.to_be_bytes();
        // In key order, as the tree takes slots: the index before members.
        for (path, value) in [
            (vec![&[4u8][..], &[1], &[7], &[2], &[0xf1]], &entry[..]),
            (vec![&[4], &[5], &[2], &[2]], &definition[..]),
        ] {
            let path: Vec<KeySegment<'_>> = path.into_iter().map(KeySegment).collect();
            tree.insert(&path, om_hbam::slot::ChunkKind::Leaf, Some(value))
                .unwrap();
        }
        let layout = Layout::decode_from_tree(&tree, 2).unwrap().unwrap();
        assert_eq!(layout.def.unwrap().flags, 0x10f3_084d);
        // A member's word of fewer than 8 bytes is its word, as the host
        // reads it: the entry's does not show through it.
        assert_eq!(stored_layout_word(Some(&[0]), Some(&entry)), Some(0));
        assert_eq!(stored_layout_word(None, Some(&entry)), Some(0x10f3_084d));
    }
    #[test]
    fn standalone_protobuf_flags_remain_available_without_member_metadata() {
        use om_hbam::{KeySegment, KeyTree};
        let mut tree = KeyTree::default();
        let definition = [0x08, 0x80, 0x04];
        tree.insert(
            &[
                KeySegment(&[4]),
                KeySegment(&[5]),
                KeySegment(&[2]),
                KeySegment(&[2]),
            ],
            om_hbam::slot::ChunkKind::Leaf,
            Some(&definition),
        )
        .unwrap();
        assert_eq!(
            Layout::decode_from_tree(&tree, 2)
                .unwrap()
                .unwrap()
                .def
                .unwrap()
                .flags,
            0x200
        );
    }

    #[test]
    fn merge_fields_are_named_and_surrounding_text_is_retained() {
        assert_eq!(
            split_merge_text(
                "Hi <<\u{1}\u{1}\u{e}0003D000020101>>!",
                &["First Name".into()]
            ),
            [
                TextRun::Literal("Hi ".into()),
                TextRun::MergeField("First Name".into()),
                TextRun::Literal("!".into())
            ]
        );
        assert_eq!(
            split_merge_text("<<plain text>>", &["Wrong".into()]),
            [TextRun::Literal("<<plain text>>".into())]
        );
        assert_eq!(
            split_merge_text(
                "<<plain>> <<\u{1}\u{3}\u{e}0003D000020101>> <<\u{1}\u{2}\u{10}0003D00002028123>>!",
                &["Related::Value".into()]
            ),
            [
                TextRun::Literal("<<plain>> <<\u{1}\u{3}\u{e}0003D000020101>> ".into()),
                TextRun::MergeField("Related::Value".into()),
                TextRun::Literal("!".into())
            ]
        );
    }
    #[test]
    fn only_complete_literal_expressions_become_static_text() {
        let mut bytes = vec![0x13, 5];
        bytes.extend(b"Title".iter().map(|b| b ^ 0x5a));
        bytes.push(0);
        assert_eq!(literal_text(&bytes).as_deref(), Some("Title"));
        bytes.push(0);
        assert!(literal_text(&bytes).is_none());
        assert!(literal_text(&[0x13, 100, 0]).is_none());
    }
    #[test]
    fn tooltip_and_placeholder_are_separate_catalog_calculations() {
        use om_hbam::{KeySegment, KeyTree, Node};
        fn node<'a>(key: &'a [u8], value: Option<&'a [u8]>, children: Vec<Node<'a>>) -> Node<'a> {
            Node {
                segment: KeySegment(key),
                value,
                push: !children.is_empty(),
                children,
            }
        }
        fn calc<'a>(key: &'a [u8], value: &'a [u8]) -> Node<'a> {
            node(key, None, vec![node(&[6], Some(value), vec![])])
        }
        let mut tooltip = vec![0x13, 7];
        tooltip.extend(b"Tooltip".iter().map(|b| b ^ 0x5a));
        tooltip.push(0);
        let mut placeholder = vec![0x13, 4];
        placeholder.extend(b"Hint".iter().map(|b| b ^ 0x5a));
        placeholder.push(0);
        // Two fields with tooltip gate set; only the second has a placeholder.
        let store = [
            0x12, 8, 8, 1, 16, 1, 24, 0x80, 0x80, 1, 0x12, 8, 8, 1, 16, 2, 24, 0x80, 0x80, 1,
        ];
        let attrs = node(
            &[5],
            None,
            vec![
                node(&[1], None, vec![calc(&[3], &tooltip)]),
                node(
                    &[2],
                    None,
                    vec![calc(&[3], &tooltip), calc(&[6], &placeholder)],
                ),
            ],
        );
        let tree = KeyTree {
            root: node(
                &[],
                None,
                vec![node(
                    &[4],
                    None,
                    vec![node(
                        &[5],
                        None,
                        vec![node(
                            &[1],
                            None,
                            vec![node(&[7], Some(&store), vec![]), attrs],
                        )],
                    )],
                )],
            ),
        };
        let layout = Layout::decode_from_tree(&tree, 1).unwrap().unwrap();
        assert_eq!(
            layout.objects.get(1).unwrap().tooltip.as_deref(),
            Some("Tooltip")
        );
        assert_eq!(layout.objects.get(1).unwrap().placeholder, None);
        assert_eq!(
            layout.objects.get(2).unwrap().placeholder.as_deref(),
            Some("Hint")
        );
    }
    #[test]
    fn merge_references_survive_the_names_stored_beside_them() {
        use om_hbam::{KeySegment, KeyTree, Node};
        fn node<'a>(key: &'a [u8], value: Option<&'a [u8]>, children: Vec<Node<'a>>) -> Node<'a> {
            Node {
                segment: KeySegment(key),
                value,
                push: !children.is_empty(),
                children,
            }
        }
        fn literal(text: &str) -> Vec<u8> {
            let scsu = om_scsu::encode_scsu(text);
            let mut out = vec![0x13, scsu.len() as u8];
            out.extend(scsu);
            out.push(0);
            out
        }
        // As on Assets' Asset Details: the token names Assignments
        // (`d0 00 05`) field 9, while the literal stored beside it says
        // "Used By", which no field of the file is called.
        let text = literal("<<\u{1}\u{2}\u{e}0003D000050109>>");
        let stored = literal("Used By");
        let store = [0x12, 4, 8, 2, 16, 1];
        let attrs = node(
            &[5],
            None,
            vec![node(
                &[1],
                None,
                vec![
                    node(&[2], None, vec![node(&[6], Some(&text), vec![])]),
                    node(
                        &[17],
                        None,
                        vec![node(
                            &[1],
                            None,
                            vec![node(&[5], None, vec![node(&[6], Some(&stored), vec![])])],
                        )],
                    ),
                ],
            )],
        );
        let tree = KeyTree {
            root: node(
                &[],
                None,
                vec![node(
                    &[4],
                    None,
                    vec![node(
                        &[5],
                        None,
                        vec![node(
                            &[1],
                            None,
                            vec![node(&[7], Some(&store), vec![]), attrs],
                        )],
                    )],
                )],
            ),
        };
        let mut layout = Layout::decode_from_tree(&tree, 1).unwrap().unwrap();
        let object = layout.objects.get(1).unwrap();
        assert_eq!(object.text.as_deref(), Some("<<Used By>>"));
        let specs = object.merge_field_specs();
        assert_eq!(
            specs
                .iter()
                .map(|s| (s.table_key, s.field_key))
                .collect::<Vec<_>>(),
            [(0xd0_0005, 9)]
        );
        let object = layout
            .objects
            .objects
            .iter_mut()
            .find(|o| o.id == 1)
            .unwrap();
        object.resolve_merge_fields(&["Assignments::Name".into()]);
        assert_eq!(object.text.as_deref(), Some("<<Assignments::Name>>"));
        assert_eq!(
            object.text_runs,
            [TextRun::MergeField("Assignments::Name".into())]
        );
    }
    #[test]
    fn object_attributes_are_keyed_by_serial_object_ids() {
        use om_hbam::{KeySegment, KeyTree, Node};
        fn node<'a>(key: &'a [u8], value: Option<&'a [u8]>, children: Vec<Node<'a>>) -> Node<'a> {
            Node {
                segment: KeySegment(key),
                value,
                push: !children.is_empty(),
                children,
            }
        }
        // A text object with id 137 (key varint 89 01): its attributes are
        // under the serial key 80 09, not under 137's big-endian bytes.
        let store = [0x12, 5, 8, 2, 16, 0x89, 0x01];
        let name: Vec<u8> = b"Header".iter().map(|b| b ^ 0x5a).collect();
        let attrs = node(
            &[5],
            None,
            vec![node(
                &[0x80, 0x09],
                None,
                vec![
                    node(&[0x0d], Some(&[0, 11]), vec![]),
                    node(&[0x10], Some(&name), vec![]),
                ],
            )],
        );
        let tree = KeyTree {
            root: node(
                &[],
                None,
                vec![node(
                    &[4],
                    None,
                    vec![node(
                        &[5],
                        None,
                        vec![node(
                            &[1],
                            None,
                            vec![node(&[7], Some(&store), vec![]), attrs],
                        )],
                    )],
                )],
            ),
        };
        let layout = Layout::decode_from_tree(&tree, 1).unwrap().unwrap();
        assert_eq!(
            layout.objects.get(137).unwrap().name.as_deref(),
            Some("Header")
        );
        assert_eq!(layout.objects.get(137).unwrap().font_height, Some(11));
        assert_eq!(layout.objects.get(137).unwrap().data_format, None);
    }
    #[test]
    fn nested_coordinates_and_cycles() {
        let parent = LayoutObject {
            id: 1,
            bounds: Some(Rect {
                top: 100.0,
                left: 200.0,
                bottom: 200.0,
                right: 300.0,
                ..Default::default()
            }),
            ..Default::default()
        };
        let child = LayoutObject {
            id: 2,
            parent: Some(1),
            bounds: Some(Rect {
                top: 3.0,
                left: 5.0,
                bottom: 13.0,
                right: 25.0,
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut layout = Layout {
            objects: ObjectStore {
                objects: vec![parent, child],
                ..Default::default()
            },
            ..Default::default()
        };
        let rect = layout.absolute_bounds(2).unwrap();
        assert_eq!(
            (rect.top, rect.left, rect.bottom, rect.right),
            (103.0, 205.0, 113.0, 225.0)
        );
        layout.objects.objects[0].parent = Some(2);
        assert!(layout.absolute_bounds(2).is_none());
    }
}
