//! File-contained theme styles and their property cascade.
//!
//! Property ids, component ids and inheritance follow `docs/re/layout-model.md`.
//! Unsupported property payloads remain in `other`; they are never discarded.
use crate::wire::{push_varints, Error, Fields, Merged, RawField, RawValue, Result, Value};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Color {
    pub red: f32,
    pub green: f32,
    pub blue: f32,
    pub alpha: f32,
}
impl Color {
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut out = Self::default();
        for f in Fields::new(buf, "ColorValue") {
            let f = f?;
            match (f.number, f.value) {
                (1, Value::Fixed32(v)) => out.red = f32::from_bits(v),
                (2, Value::Fixed32(v)) => out.green = f32::from_bits(v),
                (3, Value::Fixed32(v)) => out.blue = f32::from_bits(v),
                (4, Value::Fixed32(v)) => out.alpha = f32::from_bits(v),
                _ => {}
            }
        }
        Ok(out)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Length {
    pub value: f64,
    pub unit: i32,
    pub auto_value: i32,
    pub other: Vec<RawField>,
}
impl Length {
    /// The length in points, when it is an absolute one (layout-model.md
    /// 6.6: none, pt and px are taken as points; cm, in, mm and pica are
    /// converted). `None` for a relative unit (`%`, `em`, `line`), an
    /// automatic value other than the plain 0 or 1 classic layouts write, or
    /// a value that is not finite.
    pub fn points(&self) -> Option<f64> {
        // Classic layouts explicitly write 1 alongside a concrete point
        // size (for example 8 or 9 pt). Zero is also used by modern themes.
        // Other automatic keywords do not supply an absolute length.
        if !matches!(self.auto_value, 0 | 1) {
            return None;
        }
        let points = match self.unit {
            0 | 6 | 8 => self.value,
            2 => self.value * 72.0 / 2.54,
            3 => self.value * 72.0,
            4 => self.value * 72.0 / 25.4,
            5 => self.value * 12.0,
            _ => return None,
        };
        points.is_finite().then_some(points)
    }

    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut out = Self::default();
        for f in Fields::new(buf, "LengthValue") {
            let f = f?;
            match (f.number, f.value) {
                (1, Value::Fixed64(v)) => out.value = f64::from_bits(v),
                (2, Value::Varint(v)) => out.unit = v as i32,
                (3, Value::Varint(v)) => out.auto_value = v as i32,
                _ => out.other.push(f.into()),
            }
        }
        Ok(out)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Property {
    pub id: i32,
    pub inherited: bool,
    pub int: Option<i32>,
    pub string: Option<String>,
    pub color: Option<Color>,
    pub length: Option<Length>,
    pub other: Vec<RawField>,
}
fn string(b: &[u8], message: &'static str) -> Result<String> {
    String::from_utf8(b.to_vec()).map_err(|_| Error {
        message,
        offset: 0,
        reason: "invalid UTF-8 string",
    })
}
impl Property {
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut out = Self::default();
        let mut color = Merged::default();
        let mut length = Merged::default();
        for f in Fields::new(buf, "Property") {
            let f = f?;
            match (f.number, f.value) {
                (1, Value::Varint(v)) => out.id = v as i32,
                (2, Value::Varint(v)) => out.inherited = v != 0,
                (3, Value::Varint(v)) => out.int = Some(v as i32),
                (4, Value::Bytes(b)) => out.string = Some(string(b, "Property")?),
                (5, Value::Bytes(b)) => {
                    color.push(b);
                    if Fields::new(b, "ColorValue").any(|f| {
                        !matches!(
                            f,
                            Ok(crate::wire::Field {
                                number: 1..=4,
                                value: Value::Fixed32(_)
                            })
                        )
                    }) {
                        out.other.push(f.into())
                    }
                }
                (6, Value::Bytes(b)) => length.push(b),
                _ => out.other.push(f.into()),
            }
        }
        out.color = color.decode(Color::decode)?;
        out.length = length.decode(Length::decode)?;
        Ok(out)
    }

    /// `coordVal` (field 13), kept undecoded in [`Self::other`] so that
    /// the property re-encodes byte for byte. `None` without one, or when
    /// it does not decode.
    pub fn coord(&self) -> Option<Coord> {
        let mut merged = Merged::default();
        for field in &self.other {
            if let (13, RawValue::Bytes(bytes)) = (field.number, &field.value) {
                merged.push(bytes);
            }
        }
        merged.decode(Coord::decode).ok().flatten()
    }

    /// A corner radius in points (properties 32-35, layout-model.md 6.5):
    /// a length, or a `coordVal` whose horizontal and vertical radii are
    /// the same absolute length, as the starter solutions' themes store
    /// them (`0pt 0pt` for square corners). `None` for a relative unit, an
    /// automatic value, an elliptical corner (two different radii), or a
    /// property holding neither value.
    pub fn radius_points(&self) -> Option<f64> {
        if let Some(length) = &self.length {
            return length.points();
        }
        let coord = self.coord()?;
        let x = coord.x.as_ref()?.points()?;
        let y = coord.y.as_ref()?.points()?;
        (x == y).then_some(x)
    }
}

/// `Coord` (`Style.proto`): a horizontal and a vertical length. A corner
/// radius stores its two radii this way (`Property.coordVal`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Coord {
    pub x: Option<Length>,
    pub y: Option<Length>,
}
impl Coord {
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let (mut x, mut y) = (Merged::default(), Merged::default());
        for f in Fields::new(buf, "Coord") {
            let f = f?;
            match (f.number, f.value) {
                (1, Value::Bytes(b)) => x.push(b),
                (2, Value::Bytes(b)) => y.push(b),
                _ => {}
            }
        }
        Ok(Self {
            x: x.decode(Length::decode)?,
            y: y.decode(Length::decode)?,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ComponentStyle {
    pub component: i32,
    pub state: i32,
    pub properties: Vec<Property>,
    pub other: Vec<RawField>,
}
impl ComponentStyle {
    fn decode(buf: &[u8]) -> Result<Self> {
        let mut out = Self::default();
        for f in Fields::new(buf, "ComponentStatePropSet") {
            let f = f?;
            match (f.number, f.value) {
                (1, Value::Varint(v)) => out.component = v as i32,
                (2, Value::Varint(v)) => out.state = v as i32,
                (3, Value::Bytes(b)) => {
                    for p in Fields::new(b, "PropertySet") {
                        let p = p?;
                        if let (1, Value::Bytes(b)) = (p.number, p.value) {
                            out.properties.push(Property::decode(b)?)
                        } else {
                            out.other.push(p.into())
                        }
                    }
                }
                _ => out.other.push(f.into()),
            }
        }
        Ok(out)
    }
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StyleSet {
    pub components: Vec<ComponentStyle>,
    pub other: Vec<RawField>,
}
impl StyleSet {
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut out = Self::default();
        for f in Fields::new(buf, "StyleSet") {
            let f = f?;
            if let (1, Value::Bytes(b)) = (f.number, f.value) {
                out.components.push(ComponentStyle::decode(b)?)
            } else {
                out.other.push(f.into())
            }
        }
        Ok(out)
    }
    fn property(&self, component: i32, state: i32, id: i32) -> Option<&Property> {
        self.components
            .iter()
            .rev()
            .filter(|s| s.component == component && s.state == state)
            .find_map(|s| s.properties.iter().rev().find(|p| p.id == id))
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ObjectStyle {
    pub local_styles: Option<StyleSet>,
    pub custom_style_names: Vec<String>,
    pub object_type_id: i32,
    pub custom_style_display_names: Vec<String>,
    pub other: Vec<RawField>,
}
impl ObjectStyle {
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut out = Self::default();
        let mut local = Merged::default();
        for f in Fields::new(buf, "ObjectStyle") {
            let f = f?;
            match (f.number, f.value) {
                (1, Value::Bytes(b)) => local.push(b),
                (2, Value::Bytes(b)) => out.custom_style_names.push(string(b, "ObjectStyle")?),
                (3, Value::Varint(v)) => out.object_type_id = v as i32,
                (4, Value::Bytes(b)) => out
                    .custom_style_display_names
                    .push(string(b, "ObjectStyle")?),
                _ => out.other.push(f.into()),
            }
        }
        out.local_styles = local.decode(StyleSet::decode)?;
        Ok(out)
    }
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TypeStyle {
    pub object_types: Vec<u64>,
    pub styles: Option<StyleSet>,
    pub name: String,
    pub other: Vec<RawField>,
}
impl TypeStyle {
    fn decode(buf: &[u8], named: bool) -> Result<Self> {
        let mut out = Self::default();
        let mut styles = Merged::default();
        for f in Fields::new(
            buf,
            if named {
                "CustomStyleSet"
            } else {
                "ObjectTypeStyleSet"
            },
        ) {
            let f = f?;
            match (f.number, f.value) {
                (1, Value::Bytes(b)) if named => out.name = string(b, "CustomStyleSet")?,
                (2, Value::Bytes(b)) => styles.push(b),
                (n, v)
                    if n == if named { 3 } else { 1 }
                        && push_varints(&mut out.object_types, v, "ObjectTypeStyleSet")? => {}
                _ => out.other.push(f.into()),
            }
        }
        out.styles = styles.decode(StyleSet::decode)?;
        Ok(out)
    }
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThemeStyles {
    pub type_styles: Vec<TypeStyle>,
    pub custom_styles: Vec<TypeStyle>,
    pub deleted_custom_styles: Vec<TypeStyle>,
    pub other: Vec<RawField>,
}
impl ThemeStyles {
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut out = Self::default();
        for f in Fields::new(buf, "ThemeStyles") {
            let f = f?;
            match (f.number, f.value) {
                (1, Value::Bytes(b)) => out.type_styles.push(TypeStyle::decode(b, false)?),
                (2, Value::Bytes(b)) => out.custom_styles.push(TypeStyle::decode(b, true)?),
                (3, Value::Bytes(b)) => out.deleted_custom_styles.push(TypeStyle::decode(b, true)?),
                _ => out.other.push(f.into()),
            }
        }
        Ok(out)
    }
}

/// Frequently painted properties after the file's theme/layout/object cascade.
/// Sides are top, right, bottom, left. Missing properties let the renderer use
/// its own defaults; absent FileMaker defaults are not invented by the parser.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResolvedStyle {
    pub background: Option<Color>,
    pub color: Option<Color>,
    pub icon_color: Option<Color>,
    pub font_family: Option<String>,
    pub font_size: Option<f64>,
    /// Property 72: paragraph leading; unit 9 multiplies the rounded natural font line pitch.
    /// Retain units rather than treating relative line heights as points.
    pub line_height: Option<Length>,
    pub font_weight: Option<i32>,
    pub font_style: Option<i32>,
    /// 1 left, 2 center, 3 right (4 justify is inferred).
    pub text_align: Option<i32>,
    /// Property 75: 1 none, 3 uppercase; 2 capitalize and 4 lowercase
    /// follow the keyword ordering but have no supplied CSS/protobuf pair.
    pub text_transform: Option<i32>,
    /// Property 611: 0 top, 1 center, 2 bottom.
    pub vertical_align: Option<i32>,
    pub padding: [Option<f64>; 4],
    pub border_width: [Option<f64>; 4],
    pub border_color: [Option<Color>; 4],
    pub border_style: [Option<i32>; 4],
    pub border_radius: Option<f64>,
}

impl ResolvedStyle {
    /// Take every property `other` sets, keeping this style's others (how
    /// a conditional format applies over the cascaded style).
    pub fn overlay(&mut self, other: &ResolvedStyle) {
        macro_rules! take {
            ($($field:ident),*) => {
                $(if other.$field.is_some() {
                    self.$field = other.$field.clone();
                })*
            };
        }
        take!(
            background,
            color,
            icon_color,
            font_family,
            font_size,
            line_height,
            font_weight,
            font_style,
            text_align,
            text_transform,
            vertical_align,
            border_radius
        );
        for i in 0..4 {
            if other.padding[i].is_some() {
                self.padding[i] = other.padding[i];
            }
            if other.border_width[i].is_some() {
                self.border_width[i] = other.border_width[i];
            }
            if other.border_color[i].is_some() {
                self.border_color[i] = other.border_color[i];
            }
            if other.border_style[i].is_some() {
                self.border_style[i] = other.border_style[i];
            }
        }
    }
}

fn ancestor(t: i32) -> Option<i32> {
    match t {
        10 | 11 | 20 | 21 | 30 | 87 | 90 | 93 | 0 | 1 => None,
        31..=34 | 37..=41 | 100 | 200 => Some(30),
        35 => Some(34),
        101..=116 => Some(100),
        201..=216 => Some(200),
        50..=53 => Some(54),
        61..=67 | 94 => Some(60),
        68 => Some(61),
        71 | 72 => Some(70),
        _ => Some(1),
    }
}
fn states(state: i32) -> impl Iterator<Item = i32> {
    // Resolving a style asks for this short chain once per property.
    let (states, len) = match state {
        9 => ([9, 4, 2, 1], 4),
        10 => ([10, 7, 2, 1], 4),
        11 => ([11, 7, 3, 1], 4),
        12 => ([12, 8, 2, 1], 4),
        13 => ([13, 8, 3, 1], 4),
        16 => ([16, 7, 4, 1], 4),
        17 => ([17, 3, 4, 1], 4),
        19 => ([19, 2, 1, 0], 3),
        1 => ([1, 0, 0, 0], 1),
        s => ([s, 1, 0, 0], 2),
    };
    states.into_iter().take(len)
}

/// Resolve one property in FileMaker's state-first, then type-first order.
pub fn resolve_property<'a>(
    object: Option<&'a ObjectStyle>,
    fallback_type: i32,
    local: Option<&'a ThemeStyles>,
    theme: &'a ThemeStyles,
    component: i32,
    state: i32,
    id: i32,
) -> Option<&'a Property> {
    let object_type = object
        .map(|s| s.object_type_id)
        .filter(|t| *t != 0)
        .unwrap_or(fallback_type);
    let pick = |s: Option<&'a StyleSet>, state| s.and_then(|s| s.property(component, state, id));
    for state in states(state) {
        let mut next = Some(object_type);
        while let Some(t) = next {
            next = ancestor(t);
            if let Some(p) = pick(object.and_then(|s| s.local_styles.as_ref()), state) {
                return if p.inherited {
                    if component == 1 {
                        None
                    } else {
                        resolve_property(object, fallback_type, local, theme, 1, state, id)
                    }
                } else {
                    Some(p)
                };
            }
            if let Some(o) = object {
                for name in o.custom_style_names.iter().rev() {
                    let deleted = local
                        .is_some_and(|l| l.deleted_custom_styles.iter().any(|s| s.name == *name));
                    if deleted {
                        continue;
                    }
                    // Specific type matches precede untyped named styles.
                    for specific in [true, false] {
                        for source in [local, Some(theme)].into_iter().flatten() {
                            for s in source.custom_styles.iter().rev().filter(|s| {
                                s.name == *name
                                    && if specific {
                                        s.object_types.contains(&(t as u64))
                                    } else {
                                        true
                                    }
                            }) {
                                if let Some(p) = pick(s.styles.as_ref(), state) {
                                    return if p.inherited {
                                        if component == 1 {
                                            None
                                        } else {
                                            resolve_property(
                                                object,
                                                fallback_type,
                                                local,
                                                theme,
                                                1,
                                                state,
                                                id,
                                            )
                                        }
                                    } else {
                                        Some(p)
                                    };
                                }
                            }
                        }
                    }
                }
            }
            for source in [local, Some(theme)].into_iter().flatten() {
                for s in source
                    .type_styles
                    .iter()
                    .rev()
                    .filter(|s| s.object_types.contains(&(t as u64)))
                {
                    if let Some(p) = pick(s.styles.as_ref(), state) {
                        return if p.inherited {
                            if component == 1 {
                                None
                            } else {
                                resolve_property(object, fallback_type, local, theme, 1, state, id)
                            }
                        } else {
                            Some(p)
                        };
                    }
                }
            }
        }
    }
    None
}

pub fn resolve_style(
    object: Option<&ObjectStyle>,
    fallback_type: i32,
    local: Option<&ThemeStyles>,
    theme: &ThemeStyles,
    component: i32,
    state: i32,
) -> ResolvedStyle {
    let p = |id| resolve_property(object, fallback_type, local, theme, component, state, id);
    let length = |id| {
        p(id)
            .and_then(|p| p.length.as_ref())
            .and_then(Length::points)
    };
    ResolvedStyle {
        background: p(7).and_then(|p| p.color),
        color: p(70).and_then(|p| p.color),
        icon_color: p(514).and_then(|p| p.color),
        font_family: p(64).and_then(|p| p.string.clone()),
        font_size: length(69),
        line_height: p(72).and_then(|p| p.length.clone()),
        font_weight: p(65).and_then(|p| p.int),
        font_style: p(67).and_then(|p| p.int),
        text_align: p(74).and_then(|p| p.int),
        text_transform: p(75).and_then(|p| p.int),
        vertical_align: p(611).and_then(|p| p.int),
        padding: std::array::from_fn(|i| length(101 + i as i32)),
        border_width: std::array::from_fn(|i| length(28 + i as i32)),
        border_color: std::array::from_fn(|i| p(20 + i as i32).and_then(|p| p.color)),
        border_style: std::array::from_fn(|i| p(24 + i as i32).and_then(|p| p.int)),
        border_radius: p(35).and_then(Property::radius_points),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corner_radii_stored_as_coordinates_resolve_when_circular() {
        // `coordVal` (field 13) as Meetings' Meeting Details stores a
        // button's top-left radius: x and y of 22 pt; the starter
        // solutions' square corners store 0 pt twice.
        let length = |value: f64, unit: u8| {
            let mut out = vec![9];
            out.extend(value.to_bits().to_le_bytes());
            out.extend([16, unit, 24, 0]);
            out
        };
        let property = |x: Vec<u8>, y: Vec<u8>| {
            let mut coord = vec![10, x.len() as u8];
            coord.extend(x);
            coord.extend([18, y.len() as u8]);
            coord.extend(y);
            let mut out = vec![8, 35, 0x6a, coord.len() as u8];
            out.extend(&coord);
            (Property::decode(&out).unwrap(), coord)
        };
        let (round, coord) = property(length(22.0, 6), length(22.0, 6));
        assert_eq!(
            coord,
            [
                10, 13, 9, 0, 0, 0, 0, 0, 0, 54, 64, 16, 6, 24, 0, 18, 13, 9, 0, 0, 0, 0, 0, 0, 54,
                64, 16, 6, 24, 0
            ]
        );
        assert_eq!(round.radius_points(), Some(22.0));
        // The value stays in `other`, as stored.
        assert_eq!(
            round.other,
            [RawField {
                number: 13,
                value: RawValue::Bytes(coord)
            }]
        );
        assert_eq!(
            property(length(0.0, 6), length(0.0, 6)).0.radius_points(),
            Some(0.0)
        );
        // An elliptical corner and a relative radius have no one radius.
        assert_eq!(
            property(length(22.0, 6), length(10.0, 6)).0.radius_points(),
            None
        );
        assert_eq!(
            property(length(50.0, 1), length(50.0, 1)).0.radius_points(),
            None
        );
    }

    #[test]
    fn relative_leading_survives_resolution_and_conditional_overlays() {
        let height = Length {
            value: 0.8,
            unit: 9,
            auto_value: 1,
            ..Default::default()
        };
        let object = ObjectStyle {
            local_styles: Some(StyleSet {
                components: vec![ComponentStyle {
                    component: 1,
                    state: 1,
                    properties: vec![Property {
                        id: 72,
                        length: Some(height.clone()),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut style = resolve_style(Some(&object), 61, None, &ThemeStyles::default(), 1, 1);
        assert_eq!(style.line_height, Some(height));
        let replacement = Length {
            value: 10.0,
            unit: 6,
            ..Default::default()
        };
        style.overlay(&ResolvedStyle {
            line_height: Some(replacement.clone()),
            ..Default::default()
        });
        assert_eq!(style.line_height, Some(replacement));
    }

    fn prop(id: i32, value: i32) -> Property {
        Property {
            id,
            int: Some(value),
            ..Default::default()
        }
    }
    fn styles(component: i32, state: i32, properties: Vec<Property>) -> StyleSet {
        StyleSet {
            components: vec![ComponentStyle {
                component,
                state,
                properties,
                ..Default::default()
            }],
            ..Default::default()
        }
    }
    fn type_style(kind: u64, state: i32, value: i32) -> TypeStyle {
        TypeStyle {
            object_types: vec![kind],
            styles: Some(styles(1, state, vec![prop(65, value)])),
            ..Default::default()
        }
    }

    #[test]
    fn state_fallback_order_and_exhaustion() {
        let cases: &[(i32, &[i32])] = &[
            (9, &[9, 4, 2, 1]),
            (10, &[10, 7, 2, 1]),
            (11, &[11, 7, 3, 1]),
            (12, &[12, 8, 2, 1]),
            (13, &[13, 8, 3, 1]),
            (16, &[16, 7, 4, 1]),
            (17, &[17, 3, 4, 1]),
            (19, &[19, 2, 1]),
            (1, &[1]),
            (6, &[6, 1]),
            (0, &[0, 1]),
            (-1, &[-1, 1]),
        ];
        for &(state, chain) in cases {
            let mut theme = ThemeStyles {
                type_styles: chain.iter().map(|&s| type_style(61, s, s)).collect(),
                ..Default::default()
            };
            // An unrelated state must never be visited after exhausting the chain.
            let unrelated = if state == 0 { 5 } else { 0 };
            theme.type_styles.push(type_style(61, unrelated, 999));
            for &expected in chain {
                assert_eq!(
                    resolve_style(None, 61, None, &theme, 1, state).font_weight,
                    Some(expected),
                    "state {state}, fallback {expected}"
                );
                theme.type_styles.remove(0);
            }
            assert_eq!(
                resolve_style(None, 61, None, &theme, 1, state).font_weight,
                None,
                "state {state} exhausted"
            );
        }
    }

    #[test]
    fn custom_class_applies_to_subtype_and_precedes_type_default() {
        let theme = ThemeStyles {
            type_styles: vec![type_style(51, 1, 1)],
            custom_styles: vec![TypeStyle {
                name: "inverted".into(),
                ..type_style(54, 1, 2)
            }],
            ..Default::default()
        };
        let object = ObjectStyle {
            object_type_id: 51,
            custom_style_names: vec!["inverted".into()],
            ..Default::default()
        };
        assert_eq!(
            resolve_style(Some(&object), 51, None, &theme, 1, 1).font_weight,
            Some(2)
        );
    }

    #[test]
    fn state_precedes_type_and_normal_local_override() {
        let theme = ThemeStyles {
            type_styles: vec![type_style(1, 2, 2), type_style(61, 1, 3)],
            ..Default::default()
        };
        let object = ObjectStyle {
            object_type_id: 61,
            local_styles: Some(styles(1, 1, vec![prop(65, 7)])),
            ..Default::default()
        };
        assert_eq!(
            resolve_style(Some(&object), 61, None, &theme, 1, 2).font_weight,
            Some(2)
        );
        assert_eq!(
            resolve_style(Some(&object), 61, None, &theme, 1, 6).font_weight,
            Some(7)
        );
    }

    #[test]
    fn deleted_custom_class_and_last_applied_name() {
        let theme = ThemeStyles {
            custom_styles: vec![
                TypeStyle {
                    name: "first".into(),
                    ..type_style(61, 1, 2)
                },
                TypeStyle {
                    name: "last".into(),
                    ..type_style(61, 1, 3)
                },
            ],
            ..Default::default()
        };
        let object = ObjectStyle {
            object_type_id: 61,
            custom_style_names: vec!["first".into(), "last".into()],
            ..Default::default()
        };
        assert_eq!(
            resolve_style(Some(&object), 61, None, &theme, 1, 1).font_weight,
            Some(3)
        );
        let local = ThemeStyles {
            deleted_custom_styles: vec![TypeStyle {
                name: "last".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            resolve_style(Some(&object), 61, Some(&local), &theme, 1, 1).font_weight,
            Some(2)
        );
    }

    #[test]
    fn text_component_inheritance_uses_self_and_physical_lengths_are_points() {
        let object = ObjectStyle {
            local_styles: Some(StyleSet {
                components: vec![
                    ComponentStyle {
                        component: 1,
                        state: 1,
                        properties: vec![Property {
                            id: 69,
                            length: Some(Length {
                                value: 0.5,
                                unit: 3,
                                ..Default::default()
                            }),
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                    ComponentStyle {
                        component: 3,
                        state: 1,
                        properties: vec![Property {
                            id: 69,
                            inherited: true,
                            ..Default::default()
                        }],
                        ..Default::default()
                    },
                ],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            resolve_style(Some(&object), 61, None, &ThemeStyles::default(), 3, 1).font_size,
            Some(36.0)
        );
    }

    #[test]
    fn explicit_classic_point_sizes_keep_auto_value_one() {
        let font_size = |value, auto_value| {
            let object = ObjectStyle {
                local_styles: Some(styles(
                    1,
                    1,
                    vec![Property {
                        id: 69,
                        length: Some(Length {
                            value,
                            unit: 6,
                            auto_value,
                            ..Default::default()
                        }),
                        ..Default::default()
                    }],
                )),
                ..Default::default()
            };
            resolve_style(Some(&object), 70, None, &ThemeStyles::default(), 1, 1).font_size
        };
        assert_eq!(font_size(9.0, 1), Some(9.0));
        assert_eq!(font_size(8.0, 1), Some(8.0));
        assert_eq!(font_size(12.0, 0), Some(12.0));
        assert_eq!(font_size(9.0, 2), None);
        assert_eq!(font_size(f64::NAN, 1), None);
    }

    #[test]
    fn malformed_color_errors_and_unknown_color_fields_survive() {
        // Property 7: ColorValue {red: 1, unknown field 9: 3}.
        let property = Property::decode(&[8, 7, 42, 7, 13, 0, 0, 128, 63, 72, 3]).unwrap();
        assert_eq!(property.color.unwrap().red, 1.0);
        assert_eq!(property.other[0].number, 5);
        assert!(Property::decode(&[42, 2, 13, 0]).is_err());
        assert!(Property::decode(&[34, 1, 255]).is_err());
    }
}
