//! Calculations a layout object keeps outside its protobuf, in the layout's
//! object attribute catalog `4/5/<layout>/5/<object>` (layout-model.md 5.5,
//! calc-encoding.md 8). Each is a stored formula node (children 4 context,
//! 5 formatted, 6 Polish). Evaluation reads the context and Polish stream;
//! the Layout canvas also uses the formatted web address for its caption.
//!
//! | key | calculation |
//! |---|---|
//! | `2` | `KLabelCalc`: button, popover and panel labels |
//! | `3` | tooltip, read only when the object's flag `0x4000` is set |
//! | `6` | `KPlaceholderText` |
//! | `10` | `KSortOrder`: a portal's stored `DBSortSpec` |
//! | `17/0/5` | the parameter of a button's Perform Script action |
//! | `37` | `KFilter`: a portal's row filter |
//! | `91` | `KButtonIconSvgCalc`: a button's calculated SVG icon (FileMaker Pro 26.1, used with `buttonObj.flags` `0x4`) |
//! | `138/<n>` | `KCFConditions`: `5` the formula, `13` a `CFCondition`, `1` its predefined kind |
//! | `141` | `KHideCondition` |
//! | `142` | `KActiveObjectCalc`: the active button bar segment |
//! | `00 00/5` | a web viewer's web address (a two-byte key, layout-model.md 9.6) |

use om_formula::Formula;

use crate::style::ObjectStyle;
use crate::wire::{Fields, Merged, RawField, Result, Value};

/// Attribute keys of the object attribute catalog.
const LABEL: u64 = 2;
const TOOLTIP: u64 = 3;
const PLACEHOLDER: u64 = 6;
const ITEMS: u64 = 17;
const CONDITIONS: u64 = 138;
/// `KHideCondition`: its attribute key, also when its formula does not
/// decode ([`ObjectCalcs::undecodable`]).
pub const HIDE: u64 = 141;
/// `KActiveObjectCalc`, a button bar's active segment calculation: its
/// attribute key, also when its formula does not decode
/// ([`ObjectCalcs::undecodable`]).
pub const ACTIVE_SEGMENT: u64 = 142;
/// A web viewer's web address: the formula node under the two-byte key
/// `00 00` (layout-model.md 9.6); reported as attribute key 0 when it does
/// not decode.
const WEB_ADDRESS: &[u8] = &[0, 0];
const WEB_ADDRESS_FORMULA: u64 = 5;
/// Bound the optional display stream, as the web-address authoring path
/// bounds the bytes it accepts. A missing caption never prevents evaluation.
const MAX_WEB_ADDRESS_SOURCE_BYTES: usize = 64 * 1024;
/// `KFilter`, a portal's row filter: its attribute key, also when its
/// formula does not decode ([`ObjectCalcs::undecodable`]).
pub const PORTAL_FILTER: u64 = 37;
const PORTAL_SORT: u64 = 10;
/// `KButtonIconSvgCalc`, a button's calculated icon (layout-model.md
/// 5.3.4): its attribute key, also when its formula does not decode
/// ([`ObjectCalcs::undecodable`]).
pub const BUTTON_ICON: u64 = 91;
/// Within a conditional formatting entry.
const CONDITION_KIND: u64 = 1;
const CONDITION_FORMULA: u64 = 5;
const CONDITION_FORMAT: u64 = 13;

/// The object flag that turns the tooltip lookup on (layout-model.md 5.1).
pub const TOOLTIP_FLAG: u64 = 0x4000;

/// The calculations stored with one object. A formula whose Polish stream
/// does not decode is left out and its attribute key listed in
/// [`ObjectCalcs::undecodable`], so a renderer can tell "none" from "not
/// understood".
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ObjectCalcs {
    /// `KLabelCalc`: the label of a button, popover button or tab panel.
    pub label: Option<Formula>,
    pub tooltip: Option<Formula>,
    pub placeholder: Option<Formula>,
    pub hide: Option<Formula>,
    /// Conditional formatting, in stored order; each applies its style
    /// while its formula is true.
    pub conditions: Vec<ConditionalFormat>,
    /// The parameter of the object's Perform Script button action.
    pub script_parameter: Option<Formula>,
    /// A button bar's "Specify" active segment: the formula gives the
    /// object name of the segment to make active.
    pub active_segment: Option<Formula>,
    /// A web viewer's web address, evaluated on the object's record.
    pub web_address: Option<Formula>,
    /// The formatted web address, decoded independently of the executable
    /// formula so Layout mode can show it even if evaluation is unavailable.
    pub web_address_source: Option<Vec<om_formula::Token>>,
    /// Evaluated in the parent layout context with each portal record as
    /// `Context::portal` (calc-encoding.md 8).
    pub portal_filter: Option<Formula>,
    /// A portal's `DBSortSpec` stream. The data layer decodes and applies
    /// it; keeping its bytes here avoids coupling layout decoding to the
    /// script metadata crate, which already depends on layouts.
    pub portal_sort: Option<Vec<u8>>,
    /// `KButtonIconSvgCalc`: the SVG text of a button's "Calculated icon"
    /// (FileMaker Pro 26.1), shown when the button's flag `0x4` is set
    /// ([`crate::ButtonObject::calculated_icon`]).
    pub button_icon: Option<Formula>,
    pub undecodable: Vec<u64>,
}

impl ObjectCalcs {
    pub fn is_empty(&self) -> bool {
        *self == ObjectCalcs::default()
    }
}

/// One conditional formatting entry (`138/<n>`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ConditionalFormat {
    /// The condition; `None` when the entry stores no formula.
    pub formula: Option<Formula>,
    /// The `CFCondition` protobuf of child 13: the style applied while the
    /// condition holds.
    pub condition: CfCondition,
    /// Child 1, the predefined kind as stored, when present.
    pub kind: Option<u64>,
}

/// `FMLayoutPB.CFCondition`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CfCondition {
    pub flags: u32,
    pub style: Option<ObjectStyle>,
    pub predefined_type: u32,
    pub other: Vec<RawField>,
}

impl CfCondition {
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut out = CfCondition::default();
        let mut style = Merged::default();
        for field in Fields::new(buf, "CFCondition") {
            let field = field?;
            match (field.number, field.value) {
                (1, Value::Varint(v)) => out.flags = v as u32,
                (2, Value::Bytes(b)) => style.push(b),
                (3, Value::Varint(v)) => out.predefined_type = v as u32,
                _ => out.other.push(field.into()),
            }
        }
        out.style = style.decode(ObjectStyle::decode)?;
        Ok(out)
    }
}

/// The formula stored at `node` (its children 4 and 6). `Ok(None)` when
/// there is no Polish stream, `Err(())` when it does not decode.
fn formula(node: &om_hbam::Node<'_>) -> std::result::Result<Option<Formula>, ()> {
    let Some(polish) = node.get(6).and_then(|n| n.assembled()) else {
        return Ok(None);
    };
    let context = node.get(4).and_then(|n| n.assembled());
    Formula::decode(context.as_deref(), None, &polish)
        .map(Some)
        .map_err(|_| ())
}

/// Decode the calculations of the object attribute node `attr`. `flags` is
/// the object's protobuf flags; `has_text` says whether the object has its
/// text from key 1, in which case key 2 is that text's calculation form and
/// not a label of its own.
pub(crate) fn decode(attr: &om_hbam::Node<'_>, flags: u64, has_text: bool) -> ObjectCalcs {
    let mut calcs = ObjectCalcs::default();
    let mut undecodable = Vec::new();
    let mut take = |node: Option<&om_hbam::Node<'_>>, key: u64| -> Option<Formula> {
        match node.map(formula) {
            Some(Ok(f)) => f,
            Some(Err(())) => {
                undecodable.push(key);
                None
            }
            None => None,
        }
    };
    if !has_text {
        calcs.label = take(attr.get(LABEL), LABEL);
    }
    if flags & TOOLTIP_FLAG != 0 {
        calcs.tooltip = take(attr.get(TOOLTIP), TOOLTIP);
    }
    calcs.placeholder = take(attr.get(PLACEHOLDER), PLACEHOLDER);
    calcs.hide = take(attr.get(HIDE), HIDE);
    calcs.active_segment = take(attr.get(ACTIVE_SEGMENT), ACTIVE_SEGMENT);
    let web_address = attr
        .child(WEB_ADDRESS)
        .and_then(|node| node.get(WEB_ADDRESS_FORMULA));
    calcs.web_address = take(web_address, 0);
    calcs.web_address_source = web_address
        .and_then(|node| node.get(5))
        .and_then(|node| node.assembled_at_most(MAX_WEB_ADDRESS_SOURCE_BYTES))
        .and_then(|bytes| om_formula::formatted::decode(&bytes).ok());
    calcs.portal_filter = take(attr.get(PORTAL_FILTER), PORTAL_FILTER);
    calcs.button_icon = take(attr.get(BUTTON_ICON), BUTTON_ICON);
    calcs.portal_sort = attr
        .get(PORTAL_SORT)
        .and_then(|node| node.assembled())
        .map(|bytes| bytes.into_owned());
    calcs.script_parameter = take(
        attr.get(ITEMS)
            .and_then(|items| items.children.iter().find(|n| n.id() == Some(0)))
            .and_then(|item| item.get(5)),
        ITEMS,
    );
    if let Some(conditions) = attr.get(CONDITIONS) {
        for entry in &conditions.children {
            let condition = match entry
                .get(CONDITION_FORMAT)
                .and_then(|n| n.assembled())
                .map(|b| CfCondition::decode(&b))
            {
                Some(Ok(c)) => c,
                Some(Err(_)) => {
                    undecodable.push(CONDITIONS);
                    continue;
                }
                None => CfCondition::default(),
            };
            let formula = match entry.get(CONDITION_FORMULA).map(formula) {
                Some(Ok(f)) => f,
                Some(Err(())) => {
                    undecodable.push(CONDITIONS);
                    continue;
                }
                None => None,
            };
            calcs.conditions.push(ConditionalFormat {
                formula,
                condition,
                kind: entry.get(CONDITION_KIND).and_then(|n| n.uint_value()),
            });
        }
    }
    calcs.undecodable = undecodable;
    calcs
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_formula::{polish, NameTable};
    use om_hbam::slot::ChunkKind;
    use om_hbam::{KeySegment, KeyTree};

    fn compile(source: &str) -> Vec<u8> {
        let mut names = NameTable::default();
        names.add_occurrence(0xd0_0002, "People", 0x8002);
        names.add_field(0x8002, 2, "Score");
        let mut ctx = om_formula::Context::new(&names);
        ctx.occurrence = Some(0xd0_0002);
        ctx.support_self = true;
        polish::encode(&om_formula::compile(source, &ctx).unwrap().expr)
    }

    #[test]
    fn object_calculations_decode_from_the_attribute_catalog() {
        const CONTEXT: &[u8] = &[3, 0xd0, 0, 2];
        // CFCondition { flags 1, style {}, predefinedType 0 }.
        let format = vec![0x08, 0x01, 0x12, 0x00];
        // Object 9 of layout 1: `4/5/1/5/9/...`.
        let entries: Vec<(Vec<u8>, Vec<u8>)> = vec![
            (vec![141, 4], CONTEXT.to_vec()),
            (vec![141, 6], compile("Score > 10")),
            (vec![138, 1, 1], vec![0]),
            (vec![138, 1, 5, 6], compile("Self < 0")),
            (vec![138, 1, 13], format),
            (vec![3, 6], compile("\"Score: \" & Score")),
            (vec![17, 0, 5, 6], compile("Score * 2")),
            (vec![37, 6], compile("Score > 0")),
            (vec![10], vec![0, 0, 0, 0]),
            (vec![6, 6], vec![0xff]),
            // A 26.1 calculated button icon (`KButtonIconSvgCalc`).
            (vec![91, 6], compile("\"<svg/>\" & Score")),
        ];
        // The tree takes keys in order.
        let mut entries: Vec<(Vec<u8>, Vec<u8>)> = entries
            .into_iter()
            .map(|(rest, value)| ([&[4, 5, 1, 5, 9][..], &rest].concat(), value))
            .collect();
        entries.sort();
        let mut tree = KeyTree::default();
        for (path, value) in &entries {
            let segments: Vec<KeySegment<'_>> = path.chunks(1).map(KeySegment).collect();
            tree.insert(&segments, ChunkKind::Leaf, Some(value))
                .unwrap();
        }
        let attr = tree.at(&[4, 5, 1, 5, 9]).unwrap();

        let calcs = decode(attr, TOOLTIP_FLAG, false);
        let hide = calcs.hide.as_ref().unwrap();
        assert_eq!(hide.occurrence(), Some(0xd0_0002));
        assert_eq!(polish::encode(&hide.polish), compile("Score > 10"));
        assert_eq!(calcs.conditions.len(), 1);
        assert_eq!(calcs.conditions[0].kind, Some(0));
        assert_eq!(calcs.conditions[0].condition.flags, 1);
        assert!(calcs.conditions[0].condition.style.is_some());
        assert!(calcs.conditions[0].formula.is_some());
        assert!(calcs.tooltip.is_some());
        assert!(calcs.script_parameter.is_some());
        assert_eq!(calcs.portal_sort.as_deref(), Some(&[0, 0, 0, 0][..]));
        assert_eq!(
            polish::encode(&calcs.portal_filter.as_ref().unwrap().polish),
            compile("Score > 0")
        );
        assert_eq!(
            polish::encode(&calcs.button_icon.as_ref().unwrap().polish),
            compile("\"<svg/>\" & Score")
        );
        // The undecodable placeholder is reported, not dropped silently.
        assert_eq!(calcs.placeholder, None);
        assert_eq!(calcs.undecodable, [PLACEHOLDER]);

        // Without the tooltip flag the tooltip is not looked up.
        assert!(decode(attr, 0, false).tooltip.is_none());
        assert!(calcs.web_address.is_none());
    }

    #[test]
    fn a_web_viewers_address_is_the_formula_under_the_two_byte_key() {
        // As the Contacts template's web viewer stores it: `00 00` (one
        // segment of two bytes), then the formula node 5 with its context,
        // formatted source and Polish children.
        let object: Vec<KeySegment<'_>> = [4u8, 5, 1, 5, 7]
            .iter()
            .map(std::slice::from_ref)
            .map(KeySegment)
            .collect();
        let compiled = om_formula::compile(
            "\"https://example.com\"",
            &om_formula::Context::new(&om_formula::NoNames),
        )
        .unwrap();
        let polish = polish::encode(&compiled.expr);
        let formatted = om_formula::formatted::encode(&compiled.tokens);
        const CHILDREN: [u8; 3] = [4, 5, 6];
        for (source, stored_polish, expected) in [
            (
                formatted.as_slice(),
                polish.as_slice(),
                Some(compiled.tokens.as_slice()),
            ),
            // A damaged display stream must not change Browse evaluation.
            (&[0xff][..], polish.as_slice(), None),
            // A damaged executable stream must not hide the source.
            (
                formatted.as_slice(),
                &[][..],
                Some(compiled.tokens.as_slice()),
            ),
        ] {
            let mut tree = KeyTree::default();
            for (child, value) in CHILDREN
                .iter()
                .zip([&[3, 0xd0, 0, 2][..], source, stored_polish])
            {
                let mut path = object.clone();
                path.extend([
                    KeySegment(&[0, 0]),
                    KeySegment(&[5]),
                    KeySegment(std::slice::from_ref(child)),
                ]);
                tree.insert(&path, ChunkKind::Leaf, Some(value)).unwrap();
            }
            let calcs = decode(tree.at(&[4, 5, 1, 5, 7]).unwrap(), 0, false);
            assert_eq!(calcs.web_address_source.as_deref(), expected);
            if stored_polish.is_empty() {
                assert!(calcs.web_address.is_none());
                assert_eq!(calcs.undecodable, [0]);
            } else {
                let address = calcs.web_address.as_ref().unwrap();
                assert_eq!(address.occurrence(), Some(0xd0_0002));
                assert_eq!(polish::encode(&address.polish), polish);
                assert!(calcs.undecodable.is_empty());
            }
        }

        // A one-byte key 0 is something else.
        let mut other = KeyTree::default();
        let mut path = object.clone();
        path.extend([KeySegment(&[0]), KeySegment(&[5]), KeySegment(&[6])]);
        other.insert(&path, ChunkKind::Leaf, Some(&polish)).unwrap();
        assert!(decode(other.at(&[4, 5, 1, 5, 7]).unwrap(), 0, false)
            .web_address
            .is_none());
    }
}
