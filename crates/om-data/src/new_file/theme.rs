//! OpenMaker's theme for new files, "OpenMaker Plain": its own values,
//! written from the style model (docs/re/layout-model.md 6, 7.5), with
//! nothing taken from a Claris theme. It is stored as FileMaker stores a
//! theme a user made in the file (Meetings' custom theme is the observed
//! model): an id `com.filemaker.theme.custom.<UUID>`, the group `custom`,
//! a `ThemeStyles` `16` and a `ThemeMetadata` `17`, so FileMaker never
//! takes it for one of its installed themes or upgrades it with their
//! partials.
//!
//! It styles the layout background, the parts, fields, text and labels,
//! and buttons, every value in one table ([`STYLES`]); every other object
//! type and property takes FileMaker's built-in default (layout-model.md
//! 6.7, step 5). It sets no font family, which would be a key into the font
//! catalog `[25]`. FileMaker Pro 26 opens a file with it, names it in
//! Layout mode and the Inspector, and keeps it through its own edits
//! (#593, 2026-11-13).

use om_hbam::OwnedNode;
use om_layout::encode::{bytes, double, uint};

use super::{index, stamped, Stamp, MEMBERS, NAME, UNIQUE_ID};

/// The theme member's keys (Meetings' custom theme, Inventory's Universal
/// Touch): display name, version, locale, group, platform, id, styles and
/// metadata.
const DISPLAY_NAME: u8 = 0x0a;
const THEME_VERSION: u8 = 0x0b;
const THEME_LOCALE: u8 = 0x0c;
const GROUP: u8 = 0x0d;
const PLATFORM: u8 = 0x0f;
const STYLE_DEFS: u8 = 0x16;
const METADATA: u8 = 0x17;
/// `0f`: 1 on desktop themes (Enlightened), 2 on touch themes.
const DESKTOP: u32 = 1;

/// The theme's display name.
pub const NAME_SHOWN: &str = "OpenMaker Plain";

/// Style object types (layout-model.md 6.2).
const LAYOUT_BACKGROUND: u32 = 20;
const LAYOUT_BOX: u32 = 21;
const PART: u32 = 30;
const FIELD: u32 = 60;
const EDIT_BOX: u32 = 61;
const TEXT_BOX: u32 = 70;
const BUTTON: u32 = 80;
/// Components and states (6.3, 6.4): the object itself, and its text.
const SELF: u32 = 1;
const TEXT: u32 = 3;
const NORMAL: u32 = 1;
const HOVER: u32 = 2;
const PRESSED: u32 = 3;
const FOCUS: u32 = 4;
/// Properties (6.5).
const BACKGROUND_COLOR: u32 = 7;
const BORDER_COLOR: [u32; 4] = [20, 21, 22, 23];
const BORDER_STYLE: [u32; 4] = [24, 25, 26, 27];
const BORDER_WIDTH: [u32; 4] = [28, 29, 30, 31];
const BORDER_RADIUS: [u32; 4] = [32, 33, 34, 35];
const FONT_FAMILY: u32 = 64;
const FONT_SIZE: u32 = 69;
const COLOR: u32 = 70;
const LINE_HEIGHT: u32 = 72;
const TEXT_ALIGN: u32 = 74;
const WIDTH: u32 = 109;
const HEIGHT: u32 = 110;
const MARGIN: [u32; 4] = [105, 106, 107, 108];
const INSET: [u32; 4] = [117, 118, 119, 120];
const POSITION: u32 = 121;
const BOX_SIZING: u32 = 123;
const VERTICAL_ALIGN: u32 = 611;
/// Enumerated values as FileMaker's themes store them [O: Apex Blue in
/// Pro's blank file, Universal Touch in the starters; layout-model.md 6.5]:
/// `border-*-style: solid` 5, `text-align: center` 2 (left 1),
/// `position: absolute` 3, the box-sizing every theme's text uses, 1, and
/// `-fm-text-vertical-align: center` 1 (top 0).
const SOLID: u64 = 5;
const CENTER: u64 = 2;
const ABSOLUTE: u64 = 3;
const BOX_SIZING_TEXT: u64 = 1;
const MIDDLE: u64 = 1;
/// `LengthValue.unit` (6.6): percent, points and lines.
const PERCENT: u64 = 1;
const POINTS: u64 = 6;
const LINES: u64 = 9;

/// The theme catalog `[6]` holding the one theme, key `01`.
pub(super) fn catalog(stamp: &Stamp, uuid: &mut dyn FnMut() -> [u8; 16]) -> OwnedNode {
    let id = uuid();
    let mut member = OwnedNode::list([1], Vec::new());
    member.set_child(om_scripts::text_node(&[DISPLAY_NAME], NAME_SHOWN));
    member.set_child(OwnedNode::leaf([THEME_VERSION], 1u32.to_be_bytes()));
    member.set_child(om_scripts::text_node(&[THEME_LOCALE], "en"));
    member.set_child(om_scripts::text_node(&[GROUP], "custom"));
    member.set_child(OwnedNode::leaf([PLATFORM], DESKTOP.to_be_bytes()));
    member.set_child(om_scripts::text_node(&[NAME], &theme_id(&id)));
    member.set_child(OwnedNode::leaf([STYLE_DEFS], styles()));
    member.set_child(OwnedNode::leaf([METADATA], metadata()));
    member.set_child(OwnedNode::leaf([UNIQUE_ID], uuid().to_vec()));
    let member = stamped(member, stamp);
    OwnedNode::list(
        [super::THEMES],
        vec![
            {
                // `18` = the theme `01 01` and `20` = `01`, as Pro's blank
                // files' theme index has them.
                let mut index = index(&[(vec![1], &theme_id(&id))], false, stamp, uuid);
                index.set_child(OwnedNode::leaf([0x18], [0x01, 0x01]));
                index.set_child(OwnedNode::leaf([0x20], [0x01]));
                index
            },
            OwnedNode::list([MEMBERS], vec![member]),
        ],
    )
}

/// `com.filemaker.theme.custom.` and the UUID in capitals with `_`, as
/// FileMaker names a theme made in a file.
fn theme_id(id: &[u8; 16]) -> String {
    let hex: String = id.iter().map(|b| format!("{b:02X}")).collect();
    format!(
        "com.filemaker.theme.custom.{}_{}_{}_{}_{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// A property value.
#[derive(Clone, Copy)]
enum Value {
    /// Red, green, blue and alpha, a byte each.
    Color(u32),
    /// A length and its unit.
    Length(f64, u64),
    Int(u64),
}

/// Colors and sizes of the theme.
const WHITE: Value = Value::Color(0xffff_ffff);
const INK: Value = Value::Color(0x1a1a_1aff);
const FIELD_LINE: Value = Value::Color(0xa6a6_a6ff);
const FIELD_FOCUS: Value = Value::Color(0x3d6f_d1ff);
const BUTTON_FILL: Value = Value::Color(0xf2f2_f2ff);
const BUTTON_LINE: Value = Value::Color(0x8c8c_8cff);
const BUTTON_HOVER: Value = Value::Color(0xe6e6_e6ff);
const BUTTON_PRESSED: Value = Value::Color(0xd4d4_d4ff);
const TEXT_SIZE: Value = Value::Length(12.0, POINTS);
const EDIT_BOX_TEXT_SIZE: Value = Value::Length(16.0, POINTS);
const LINE: Value = Value::Length(1.0, POINTS);
const CORNER: Value = Value::Length(3.0, POINTS);
const SOLID_LINE: Value = Value::Int(SOLID);
const ONE_LINE: Value = Value::Length(1.0, LINES);
const FULL: Value = Value::Length(100.0, PERCENT);
const NO_INSET: Value = Value::Length(0.0, POINTS);
const TEXT_MARGIN_Y: Value = Value::Length(2.0, POINTS);
const TEXT_MARGIN_X: Value = Value::Length(4.0, POINTS);
/// Font `00 00` of the file's font catalog, "arial,sans-serif"
/// (new_file.rs `fonts`).
const DEFAULT_FONT: Value = Value::Int(0);

/// Every style of OpenMaker Plain, one row per object type, component,
/// state and property: add a row to style more. Labels take the text's
/// style (`label` inherits `text_box`), header, body and footer the part's
/// (layout-model.md 6.2). Every object that shows text gives its text
/// component a size: FileMaker Pro draws no text where the theme leaves
/// it out (#593: an edit box's border drawn, its label and value not).
#[rustfmt::skip]
const STYLES: &[(u32, u32, u32, u32, Value)] = &[
    (LAYOUT_BACKGROUND, SELF, NORMAL, BACKGROUND_COLOR, WHITE),
    (LAYOUT_BOX, SELF, NORMAL, BACKGROUND_COLOR, WHITE),
    (PART, SELF, NORMAL, BACKGROUND_COLOR, WHITE),
    // Fields: white, a one-point gray border, blue in focus.
    (FIELD, SELF, NORMAL, BACKGROUND_COLOR, WHITE),
    (FIELD, SELF, NORMAL, BORDER_COLOR[0], FIELD_LINE),
    (FIELD, SELF, NORMAL, BORDER_COLOR[1], FIELD_LINE),
    (FIELD, SELF, NORMAL, BORDER_COLOR[2], FIELD_LINE),
    (FIELD, SELF, NORMAL, BORDER_COLOR[3], FIELD_LINE),
    (FIELD, SELF, NORMAL, BORDER_STYLE[0], SOLID_LINE),
    (FIELD, SELF, NORMAL, BORDER_STYLE[1], SOLID_LINE),
    (FIELD, SELF, NORMAL, BORDER_STYLE[2], SOLID_LINE),
    (FIELD, SELF, NORMAL, BORDER_STYLE[3], SOLID_LINE),
    (FIELD, SELF, NORMAL, BORDER_WIDTH[0], LINE),
    (FIELD, SELF, NORMAL, BORDER_WIDTH[1], LINE),
    (FIELD, SELF, NORMAL, BORDER_WIDTH[2], LINE),
    (FIELD, SELF, NORMAL, BORDER_WIDTH[3], LINE),
    (FIELD, SELF, NORMAL, COLOR, INK),
    (FIELD, SELF, NORMAL, FONT_FAMILY, DEFAULT_FONT),
    (FIELD, SELF, NORMAL, FONT_SIZE, TEXT_SIZE),
    (FIELD, SELF, NORMAL, LINE_HEIGHT, ONE_LINE),
    (FIELD, SELF, FOCUS, BORDER_COLOR[0], FIELD_FOCUS),
    (FIELD, SELF, FOCUS, BORDER_COLOR[1], FIELD_FOCUS),
    (FIELD, SELF, FOCUS, BORDER_COLOR[2], FIELD_FOCUS),
    (FIELD, SELF, FOCUS, BORDER_COLOR[3], FIELD_FOCUS),
    // An edit box's text fills the box, its spacing in margins and none in
    // insets, as Apex Blue's: FileMaker Pro sizes a field it places from
    // the borders, the margins and one line, and draws the text inside the
    // insets too, so insets cut it (#593, #853; layout-model.md, "Text
    // margins, insets and a placed field's height").
    // Its text is 16 pt, as Apex Blue's edit box: Pro sizes a Table view
    // row from the edit box's font but draws the cells at the field's
    // 12 pt, so a 12 pt edit box got a row too short for its own cells
    // (#853; layout-model.md 10.14).
    (EDIT_BOX, SELF, NORMAL, FONT_SIZE, EDIT_BOX_TEXT_SIZE),
    (EDIT_BOX, SELF, NORMAL, LINE_HEIGHT, ONE_LINE),
    (EDIT_BOX, TEXT, NORMAL, POSITION, Value::Int(ABSOLUTE)),
    (EDIT_BOX, TEXT, NORMAL, INSET[0], NO_INSET),
    (EDIT_BOX, TEXT, NORMAL, INSET[1], NO_INSET),
    (EDIT_BOX, TEXT, NORMAL, INSET[2], NO_INSET),
    (EDIT_BOX, TEXT, NORMAL, INSET[3], NO_INSET),
    (EDIT_BOX, TEXT, NORMAL, MARGIN[0], TEXT_MARGIN_Y),
    (EDIT_BOX, TEXT, NORMAL, MARGIN[1], TEXT_MARGIN_X),
    (EDIT_BOX, TEXT, NORMAL, MARGIN[2], TEXT_MARGIN_Y),
    (EDIT_BOX, TEXT, NORMAL, MARGIN[3], TEXT_MARGIN_X),
    (EDIT_BOX, TEXT, NORMAL, BOX_SIZING, Value::Int(BOX_SIZING_TEXT)),
    // Text and labels, their text the object's full height.
    (TEXT_BOX, SELF, NORMAL, COLOR, INK),
    (TEXT_BOX, SELF, NORMAL, FONT_FAMILY, DEFAULT_FONT),
    (TEXT_BOX, SELF, NORMAL, FONT_SIZE, TEXT_SIZE),
    (TEXT_BOX, SELF, NORMAL, LINE_HEIGHT, ONE_LINE),
    (TEXT_BOX, TEXT, NORMAL, HEIGHT, FULL),
    (TEXT_BOX, TEXT, NORMAL, BOX_SIZING, Value::Int(BOX_SIZING_TEXT)),
    // Buttons: light gray, a gray border and 3-point corners, darker when
    // hovered and pressed.
    (BUTTON, SELF, NORMAL, BACKGROUND_COLOR, BUTTON_FILL),
    (BUTTON, SELF, NORMAL, BORDER_COLOR[0], BUTTON_LINE),
    (BUTTON, SELF, NORMAL, BORDER_COLOR[1], BUTTON_LINE),
    (BUTTON, SELF, NORMAL, BORDER_COLOR[2], BUTTON_LINE),
    (BUTTON, SELF, NORMAL, BORDER_COLOR[3], BUTTON_LINE),
    (BUTTON, SELF, NORMAL, BORDER_STYLE[0], SOLID_LINE),
    (BUTTON, SELF, NORMAL, BORDER_STYLE[1], SOLID_LINE),
    (BUTTON, SELF, NORMAL, BORDER_STYLE[2], SOLID_LINE),
    (BUTTON, SELF, NORMAL, BORDER_STYLE[3], SOLID_LINE),
    (BUTTON, SELF, NORMAL, BORDER_WIDTH[0], LINE),
    (BUTTON, SELF, NORMAL, BORDER_WIDTH[1], LINE),
    (BUTTON, SELF, NORMAL, BORDER_WIDTH[2], LINE),
    (BUTTON, SELF, NORMAL, BORDER_WIDTH[3], LINE),
    (BUTTON, SELF, NORMAL, BORDER_RADIUS[0], CORNER),
    (BUTTON, SELF, NORMAL, BORDER_RADIUS[1], CORNER),
    (BUTTON, SELF, NORMAL, BORDER_RADIUS[2], CORNER),
    (BUTTON, SELF, NORMAL, BORDER_RADIUS[3], CORNER),
    (BUTTON, SELF, NORMAL, COLOR, INK),
    (BUTTON, SELF, NORMAL, FONT_FAMILY, DEFAULT_FONT),
    (BUTTON, SELF, NORMAL, FONT_SIZE, TEXT_SIZE),
    (BUTTON, SELF, NORMAL, LINE_HEIGHT, ONE_LINE),
    (BUTTON, SELF, NORMAL, TEXT_ALIGN, Value::Int(CENTER)),
    (BUTTON, SELF, NORMAL, VERTICAL_ALIGN, Value::Int(MIDDLE)),
    (BUTTON, TEXT, NORMAL, WIDTH, FULL),
    (BUTTON, TEXT, NORMAL, HEIGHT, FULL),
    (BUTTON, TEXT, NORMAL, BOX_SIZING, Value::Int(BOX_SIZING_TEXT)),
    (BUTTON, SELF, HOVER, BACKGROUND_COLOR, BUTTON_HOVER),
    (BUTTON, SELF, PRESSED, BACKGROUND_COLOR, BUTTON_PRESSED),
];

/// The theme's `ThemeStyles`: a type style set per object type in the
/// table's order, each with a property set per component and state.
fn styles() -> Vec<u8> {
    fn first_seen(values: impl Iterator<Item = u32>) -> Vec<u32> {
        let mut seen = Vec::new();
        for value in values {
            if !seen.contains(&value) {
                seen.push(value);
            }
        }
        seen
    }
    let mut out = Vec::new();
    for object_type in first_seen(STYLES.iter().map(|r| r.0)) {
        let rows: Vec<_> = STYLES.iter().filter(|r| r.0 == object_type).collect();
        let mut style_set = Vec::new();
        let pairs = first_seen(rows.iter().map(|r| r.1 << 16 | r.2));
        for pair in pairs {
            let (component, state) = (pair >> 16, pair & 0xffff);
            let mut propset = Vec::new();
            for (.., id, value) in rows.iter().filter(|r| r.1 == component && r.2 == state) {
                bytes(1, &property(*id, value), &mut propset);
            }
            let mut entry = Vec::new();
            uint(1, u64::from(component), &mut entry);
            uint(2, u64::from(state), &mut entry);
            bytes(3, &propset, &mut entry);
            bytes(1, &entry, &mut style_set);
        }
        let mut set = Vec::new();
        uint(1, u64::from(object_type), &mut set);
        bytes(2, &style_set, &mut set);
        bytes(1, &set, &mut out);
    }
    out
}

/// One `Property`: its id and value (Style.proto).
fn property(id: u32, value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    uint(1, u64::from(id), &mut out);
    match value {
        Value::Int(v) => uint(3, *v, &mut out),
        Value::Color(rgba) => {
            let mut color = Vec::new();
            for (number, shift) in [(1u32, 24), (2, 16), (3, 8), (4, 0)] {
                let channel = ((rgba >> shift) & 0xff) as f32 / 255.0;
                color.push(((number << 3) | 5) as u8);
                color.extend(channel.to_le_bytes());
            }
            bytes(5, &color, &mut out);
        }
        Value::Length(value, unit) => {
            let mut length = Vec::new();
            double(1, *value, &mut length);
            uint(2, *unit, &mut length);
            bytes(6, &length, &mut out);
        }
    }
    out
}

/// The theme's metadata: the layout builder's base font size, which
/// FileMaker gives text with no font size of its own (layout-model.md 6.7,
/// 7.4).
fn metadata() -> Vec<u8> {
    let mut pair = Vec::new();
    bytes(1, b"kBaseFontSize", &mut pair);
    bytes(2, b"12", &mut pair);
    let mut set = Vec::new();
    bytes(1, b"layoutbuilder", &mut set);
    bytes(2, &pair, &mut set);
    let mut out = Vec::new();
    bytes(1, &set, &mut out);
    out
}
